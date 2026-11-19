#!/usr/bin/env python3
"""MCP stdio JSON-RPC 2.0 latency, payload, RSS, and inotify adapter-load benchmark for ACDD v0.6.2."""

from __future__ import annotations

import argparse
import ctypes
import hashlib
import json
import os
import pathlib
import platform
import statistics
import struct
import subprocess
import sys
import tempfile
import threading
import time
from dataclasses import dataclass
from typing import Any

from benchmark_profile import (
    BenchmarkProfile,
    load_profile,
    resolve_binary,
    run_cli_json,
    summarize_series,
)


PROTOCOL_VERSION = "2025-03-26"
IN_OPEN = 0x20
IN_CLOSE_NOWRITE = 0x10


def _process_tree(root_pid: int) -> set[int]:
    pending = [root_pid]
    pids = {root_pid}
    while pending:
        parent = pending.pop()
        try:
            children_text = pathlib.Path(f"/proc/{parent}/task/{parent}/children").read_text()
            children = [int(pid) for pid in children_text.split()]
        except (FileNotFoundError, PermissionError, ProcessLookupError, ValueError):
            continue
        for child in children:
            if child not in pids:
                pids.add(child)
                pending.append(child)
    return pids


def _process_cpu_seconds(pid: int) -> float:
    total_ns = 0
    task_dir = pathlib.Path(f"/proc/{pid}/task")
    try:
        tasks = list(task_dir.iterdir())
    except (FileNotFoundError, PermissionError, ProcessLookupError):
        tasks = []
    for task in tasks:
        try:
            total_ns += int((task / "schedstat").read_text().split()[0])
        except (FileNotFoundError, PermissionError, ProcessLookupError, IndexError, ValueError):
            continue
    if not tasks:
        try:
            total_ns = int(pathlib.Path(f"/proc/{pid}/schedstat").read_text().split()[0])
        except (FileNotFoundError, PermissionError, ProcessLookupError, IndexError, ValueError):
            pass
    return total_ns / 1e9


def _proc_sample(root_pid: int) -> tuple[float, int, int]:
    """Return process-tree CPU seconds, VmRSS KiB, and VmHWM KiB."""
    cpu_seconds = 0.0
    rss_kib = 0
    hwm_kib = 0
    for member in _process_tree(root_pid):
        try:
            cpu_seconds += _process_cpu_seconds(member)
            lines = pathlib.Path(f"/proc/{member}/status").read_text().splitlines()
            for line in lines:
                if line.startswith("VmRSS:"):
                    rss_kib += int(line.split()[1])
                elif line.startswith("VmHWM:"):
                    hwm_kib += int(line.split()[1])
        except (FileNotFoundError, PermissionError, ProcessLookupError, IndexError, ValueError):
            continue
    return cpu_seconds, rss_kib, hwm_kib


def _watch_inotify(path: pathlib.Path) -> int:
    libc = ctypes.CDLL(None, use_errno=True)
    libc.inotify_init1.argtypes = [ctypes.c_int]
    libc.inotify_init1.restype = ctypes.c_int
    libc.inotify_add_watch.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_uint32]
    libc.inotify_add_watch.restype = ctypes.c_int
    fd = libc.inotify_init1(os.O_NONBLOCK | os.O_CLOEXEC)
    if fd < 0:
        raise OSError(ctypes.get_errno(), "inotify_init1 failed")
    if libc.inotify_add_watch(fd, os.fsencode(path), IN_OPEN | IN_CLOSE_NOWRITE) < 0:
        err = OSError(ctypes.get_errno(), "inotify_add_watch failed")
        os.close(fd)
        raise err
    return fd


def _drain_inotify_opens(fd: int) -> int:
    opened = 0
    while True:
        try:
            data = os.read(fd, 65536)
        except BlockingIOError:
            return opened
        if not data:
            return opened
        offset = 0
        while offset < len(data):
            _, mask, _, name_len = struct.unpack_from("iIII", data, offset)
            if mask & IN_OPEN:
                opened += 1
            offset += 16 + name_len


@dataclass
class McpSample:
    wall_ms: float
    cpu_ms: float
    rss_peak_kib: int
    rss_hwm_kib: int
    wire_bytes: int
    content_bytes: int
    error: str | None


class McpClient:
    def __init__(self, binary: pathlib.Path, workspace_root: pathlib.Path, db_path: pathlib.Path):
        started = time.perf_counter_ns()
        command = [
            str(binary),
            "--root",
            str(workspace_root),
            "--db",
            str(db_path),
            "--profile",
            "code",
            "serve",
        ]
        self.process = subprocess.Popen(
            command,
            cwd=workspace_root,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
        )
        self.next_id = 0
        init_resp, _, _ = self.request(
            "initialize",
            {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "acdd-v062-benchmark", "version": "1"},
            },
        )
        if init_resp.get("error"):
            raise RuntimeError(f"MCP initialize failed: {init_resp['error']}")
        self.notify("notifications/initialized")
        self.initialize_ms = (time.perf_counter_ns() - started) / 1e6

    def notify(self, method: str) -> None:
        assert self.process.stdin is not None
        self.process.stdin.write(json.dumps({"jsonrpc": "2.0", "method": method}) + "\n")
        self.process.stdin.flush()

    def request(self, method: str, params: dict[str, Any]) -> tuple[dict[str, Any], float, int]:
        self.next_id += 1
        request_id = self.next_id
        payload = {"jsonrpc": "2.0", "id": request_id, "method": method, "params": params}
        assert self.process.stdin is not None and self.process.stdout is not None
        started = time.perf_counter_ns()
        self.process.stdin.write(json.dumps(payload, separators=(",", ":")) + "\n")
        self.process.stdin.flush()
        wire = ""
        while True:
            line = self.process.stdout.readline()
            if not line:
                stderr = self.process.stderr.read() if self.process.stderr else ""
                raise RuntimeError(f"acdd serve exited unexpectedly; stderr={stderr[-2000:]}")
            wire += line
            try:
                response = json.loads(line)
            except json.JSONDecodeError:
                continue
            if response.get("id") == request_id:
                return response, (time.perf_counter_ns() - started) / 1e6, len(wire.encode())

    def call(self, name: str, arguments: dict[str, Any]) -> McpSample:
        before_cpu, before_rss, _ = _proc_sample(self.process.pid)
        peak_rss = [before_rss]
        peak_cpu = [before_cpu]
        stop = threading.Event()

        def sampler_loop() -> None:
            while not stop.is_set():
                cpu, rss, _ = _proc_sample(self.process.pid)
                peak_rss[0] = max(peak_rss[0], rss)
                peak_cpu[0] = max(peak_cpu[0], cpu)
                stop.wait(0.005)

        sampler = threading.Thread(target=sampler_loop, daemon=True)
        sampler.start()
        response, wall_ms, wire_bytes = self.request(
            "tools/call", {"name": name, "arguments": arguments}
        )
        stop.set()
        sampler.join()

        after_cpu, after_rss, hwm = _proc_sample(self.process.pid)
        result = response.get("result", {})
        text = "\n".join(
            part.get("text", "") for part in result.get("content", []) if isinstance(part, dict)
        )
        error = None
        if response.get("error"):
            error = json.dumps(response["error"])[:1000]
        elif result.get("isError"):
            error = text[:1000] or "tool returned isError=true"

        return McpSample(
            wall_ms=wall_ms,
            cpu_ms=max(0.0, (max(after_cpu, peak_cpu[0]) - before_cpu) * 1000.0),
            rss_peak_kib=max(peak_rss[0], after_rss),
            rss_hwm_kib=hwm,
            wire_bytes=wire_bytes,
            content_bytes=len(text.encode("utf-8")),
            error=error,
        )

    def close(self) -> None:
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=3)

    def __enter__(self) -> McpClient:
        return self

    def __exit__(self, *_: object) -> None:
        self.close()


def _check_adapter_warm_opens(
    client: McpClient,
    config_path: pathlib.Path,
    sample_tool: str,
    sample_args: dict[str, Any],
) -> dict[str, Any]:
    if not config_path.is_file() or platform.system() != "Linux":
        return {"supported": False, "warm_opens": None}
    fd = _watch_inotify(config_path)
    try:
        warmup = client.call(sample_tool, sample_args)
        if warmup.error:
            raise RuntimeError(f"inotify warmup call failed: {warmup.error}")
        initial_opens = _drain_inotify_opens(fd)
        warm = client.call(sample_tool, sample_args)
        if warm.error:
            raise RuntimeError(f"inotify warm call failed: {warm.error}")
        warm_opens = _drain_inotify_opens(fd)
        return {
            "supported": True,
            "tool": sample_tool,
            "initial_opens": initial_opens,
            "warm_opens": warm_opens,
            "single_load_on_warm_call": warm_opens == 1,
        }
    finally:
        os.close(fd)


def benchmark_mcp(
    profile: BenchmarkProfile,
    binary: str | pathlib.Path | None = None,
    repeats: int = 5,
    scenario_id: str | None = None,
) -> dict[str, Any]:
    if repeats < 1:
        raise ValueError("repeats must be at least 1")
    acdd_bin = resolve_binary(binary or profile.acdd_binary)
    selected = [
        s for s in profile.scenarios if scenario_id is None or s["id"] == scenario_id
    ]
    if not selected:
        raise ValueError(f"no scenarios matched filter: {scenario_id!r}")

    with tempfile.TemporaryDirectory(prefix="acdd-mcp-bench-") as directory:
        work = pathlib.Path(directory)
        db_path = work / "index.sqlite"
        build_info = run_cli_json(
            binary=acdd_bin,
            repo=profile.workspace_root,
            db=db_path,
            subcommand=["build"],
            work=work,
            extra_env={"ACDD_EXTRACTION_CACHE_DIR": str(work / "extraction-cache")},
            label="mcp-prep-build",
        )

        scenario_reports: list[dict[str, Any]] = []
        max_rss_kib = 0
        max_hwm_kib = 0

        with McpClient(acdd_bin, profile.workspace_root, db_path) as client:
            inotify_probe = _check_adapter_warm_opens(
                client,
                profile.workspace_root / "acdd.yaml",
                selected[0]["tool"],
                selected[0]["arguments"],
            )

            for item in selected:
                tool_name = item["tool"]
                arguments = item["arguments"]

                warmup = client.call(tool_name, arguments)
                if warmup.error:
                    raise RuntimeError(
                        f"scenario {item['id']} ({tool_name}) failed warmup: {warmup.error}"
                    )

                samples: list[McpSample] = []
                for _ in range(repeats):
                    sample = client.call(tool_name, arguments)
                    if sample.error:
                        raise RuntimeError(
                            f"scenario {item['id']} ({tool_name}) failed: {sample.error}"
                        )
                    samples.append(sample)
                    max_rss_kib = max(max_rss_kib, sample.rss_peak_kib)
                    max_hwm_kib = max(max_hwm_kib, sample.rss_hwm_kib)

                wall_stats = summarize_series([s.wall_ms for s in samples], decimals=3)
                cpu_stats = summarize_series([s.cpu_ms for s in samples], decimals=3)
                payload_median = int(statistics.median(s.content_bytes for s in samples))
                wire_median = int(statistics.median(s.wire_bytes for s in samples))
                peak_rss_mib = round(max(s.rss_peak_kib for s in samples) / 1024.0, 3)

                scenario_reports.append(
                    {
                        "id": item["id"],
                        "scenario": item["scenario"],
                        "tool": tool_name,
                        "arguments": arguments,
                        "repeats": len(samples),
                        "errors": 0,
                        "p50_ms": wall_stats["median"],
                        "p95_ms": wall_stats["p95"],
                        "mean_ms": wall_stats["mean"],
                        "min_ms": wall_stats["min"],
                        "max_ms": wall_stats["max"],
                        "cpu_ms_median": cpu_stats["median"],
                        "payload_bytes_median": payload_median,
                        "wire_bytes_median": wire_median,
                        "peak_rss_mib": peak_rss_mib,
                    }
                )
                print(
                    f"[mcp] {item['id']:<24} {tool_name:<22} "
                    f"p50={wall_stats['median']:7.2f}ms p95={wall_stats['p95']:7.2f}ms "
                    f"payload={payload_median:6d}B rss={peak_rss_mib:6.2f}MiB",
                    file=sys.stderr,
                    flush=True,
                )

    return {
        "profile": str(profile.path),
        "profile_name": profile.name,
        "workspace": str(profile.workspace_root),
        "binary": str(acdd_bin),
        "binary_sha256": hashlib.sha256(acdd_bin.read_bytes()).hexdigest(),
        "prep_build_wall_ms": round(build_info["wall_ms"], 3),
        "initialize_ms": round(client.initialize_ms, 3),
        "peak_rss_mib": round(max(max_rss_kib, max_hwm_kib) / 1024.0, 3),
        "inotify_acdd_yaml": inotify_probe,
        "scenario_count": len(scenario_reports),
        "total_errors": 0,
        "scenarios": scenario_reports,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", type=pathlib.Path, required=True, help="path to JSON profile")
    parser.add_argument("--workspace", type=pathlib.Path, help="override workspace root")
    parser.add_argument("--binary", help="override ACDD binary path")
    parser.add_argument("--repeats", type=int, default=5, help="warm repetitions per scenario (default: 5)")
    parser.add_argument("--scenario", help="optional single scenario id to run")
    parser.add_argument("--output", type=pathlib.Path, help="optional JSON report output path")
    args = parser.parse_args()

    try:
        profile = load_profile(args.profile, args.workspace)
        report = benchmark_mcp(
            profile=profile,
            binary=args.binary,
            repeats=args.repeats,
            scenario_id=args.scenario,
        )
    except (OSError, RuntimeError, ValueError) as error:
        parser.error(str(error))

    rendered = json.dumps(report, indent=2, sort_keys=True)
    if args.output is not None:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(rendered + "\n")
    print(rendered)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
