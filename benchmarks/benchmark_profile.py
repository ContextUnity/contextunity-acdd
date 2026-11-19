"""Shared JSON profile loading, workspace isolation, and CLI timing helpers."""

from __future__ import annotations

import json
import math
import os
import pathlib
import re
import shutil
import statistics
import subprocess
import time
from dataclasses import dataclass
from typing import Any


ROOT = pathlib.Path(__file__).resolve().parent.parent


def _primary_repo_root() -> pathlib.Path:
    probe = subprocess.run(
        ["git", "-C", str(ROOT), "rev-parse", "--git-common-dir"],
        check=False,
        capture_output=True,
        text=True,
    )
    if probe.returncode == 0:
        common = pathlib.Path(probe.stdout.strip())
        if not common.is_absolute():
            common = (ROOT / common).resolve()
        else:
            common = common.resolve()
        if common.name == ".git":
            return common.parent
    return ROOT

DEFAULT_EXCLUDED_DIRECTORIES = {
    ".agents",
    ".benchmarks",
    ".codex",
    ".contextunity",
    ".acdd",
    ".git",
    ".kilo",
    ".mypy_cache",
    ".next",
    ".pi",
    ".playwright-mcp",
    ".pytest_cache",
    ".ruff_cache",
    ".venv",
    "__pycache__",
    "build",
    "coverage",
    "dist",
    "node_modules",
    "target",
    "vendor",
    "venv",
}

BENCHMARK_FILES = {
    "benchmark_profile.py",
    "cold_build_benchmark.py",
    "cold_coverage_metrics.py",
    "delta_benchmark.py",
    "mcp_latency_benchmark.py",
    "run_benchmarks.py",
}


@dataclass(frozen=True)
class BenchmarkProfile:
    path: pathlib.Path
    name: str
    project: str
    workspace_root: pathlib.Path
    acdd_binary: str
    scope_note: str
    excluded_directories: frozenset[str]
    scenarios: list[dict[str, Any]]


def _require_string(value: object, label: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise ValueError(f"profile field {label!r} must be a non-empty string")
    return value


def _interpolate(value: Any, project: str) -> Any:
    if isinstance(value, dict):
        return {key: _interpolate(item, project) for key, item in value.items()}
    if isinstance(value, list):
        return [_interpolate(item, project) for item in value]
    if isinstance(value, str):
        return value.replace("${project}", project)
    return value


def _parse_call(value: object, label: str, project: str) -> tuple[str, dict[str, Any]]:
    if not isinstance(value, dict):
        raise ValueError(f"{label} must be an object with tool and arguments")
    tool = _require_string(value.get("tool"), f"{label}.tool")
    arguments = value.get("arguments", {})
    if not isinstance(arguments, dict):
        raise ValueError(f"{label}.arguments must be an object")
    return tool, _interpolate(arguments, project)


def _binary_path(value: object, command_name: str, profile_directory: pathlib.Path) -> str:
    if value is not None:
        binary = pathlib.Path(_require_string(value, f"{command_name} binary")).expanduser()
        if binary.is_absolute() or "/" in str(binary):
            if not binary.is_absolute():
                binary = profile_directory / binary
            return str(binary.resolve())
        return str(binary)
    default_release = ROOT / "target" / "release" / command_name
    if default_release.is_file() and os.access(default_release, os.X_OK):
        return str(default_release)
    return shutil.which(command_name) or command_name


def resolve_binary(value: str | pathlib.Path) -> pathlib.Path:
    raw = str(value)
    candidate = pathlib.Path(raw).expanduser()
    if candidate.is_absolute() or "/" in raw:
        binary = candidate.resolve()
    else:
        found = shutil.which(raw)
        if found is None:
            raise ValueError(f"binary is not on PATH: {raw}")
        binary = pathlib.Path(found).resolve()
    if not binary.is_file() or not os.access(binary, os.X_OK):
        raise ValueError(f"binary is not an executable file: {binary}")
    return binary


def load_profile(
    profile_path: pathlib.Path,
    workspace_override: pathlib.Path | None = None,
) -> BenchmarkProfile:
    path = profile_path.expanduser().resolve()
    try:
        raw = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as error:
        raise ValueError(f"cannot load benchmark profile {path}: {error}") from error
    if (
        not isinstance(raw, dict)
        or not isinstance(raw.get("schema_version"), int)
        or isinstance(raw.get("schema_version"), bool)
        or raw.get("schema_version") != 1
    ):
        raise ValueError("benchmark profile requires schema_version: 1")

    name = _require_string(raw.get("name"), "name")
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]{0,99}", name):
        raise ValueError("profile name must contain only letters, digits, dot, underscore, or hyphen")
    project = _require_string(raw.get("project"), "project")
    configured_workspace = workspace_override if workspace_override is not None else raw.get("workspace_root")
    if configured_workspace is None:
        raise ValueError("set workspace_root in the profile or pass --workspace")
    if not isinstance(configured_workspace, (str, pathlib.Path)):
        raise ValueError("workspace_root must be a path string")
    workspace_root = pathlib.Path(configured_workspace).expanduser()
    if not workspace_root.is_absolute():
        base = pathlib.Path.cwd() if workspace_override is not None else _primary_repo_root()
        workspace_root = base / workspace_root
    workspace_root = workspace_root.resolve()
    if not workspace_root.is_dir():
        raise ValueError(f"workspace root is not a directory: {workspace_root}")

    raw_scenarios = raw.get("scenarios")
    if not isinstance(raw_scenarios, list) or not raw_scenarios:
        raise ValueError("profile scenarios must be a non-empty array")
    scenarios: list[dict[str, Any]] = []
    identifiers: set[str] = set()
    for index, item in enumerate(raw_scenarios):
        label = f"scenarios[{index}]"
        if not isinstance(item, dict):
            raise ValueError(f"{label} must be an object")
        scenario_id = _require_string(item.get("id"), f"{label}.id")
        if scenario_id in identifiers:
            raise ValueError(f"duplicate scenario id: {scenario_id}")
        identifiers.add(scenario_id)
        scenario_name = _require_string(item.get("scenario"), f"{label}.scenario")
        call_spec = item.get("acdd") if "acdd" in item else item
        tool, arguments = _parse_call(call_spec, f"{label}", project)
        note = item.get("note", "")
        if not isinstance(note, str):
            raise ValueError(f"{label}.note must be a string")
        scenarios.append(
            {
                "id": scenario_id,
                "scenario": scenario_name,
                "tool": tool,
                "arguments": arguments,
                "note": note,
            }
        )

    exclusions = raw.get("exclude_directories", [])
    if not isinstance(exclusions, list) or not all(
        isinstance(entry, str) and entry and "/" not in entry and "\\" not in entry
        for entry in exclusions
    ):
        raise ValueError("exclude_directories must be an array of directory names")
    scope_note = raw.get("scope_note", "")
    if not isinstance(scope_note, str):
        raise ValueError("scope_note must be a string")

    return BenchmarkProfile(
        path=path,
        name=name,
        project=project,
        workspace_root=workspace_root,
        acdd_binary=_binary_path(raw.get("acdd_binary"), "acdd", path.parent),
        scope_note=scope_note,
        excluded_directories=frozenset(DEFAULT_EXCLUDED_DIRECTORIES | set(exclusions)),
        scenarios=scenarios,
    )


def copy_workspace_snapshot(
    workspace_root: pathlib.Path,
    destination: pathlib.Path,
    excluded_directories: frozenset[str] = frozenset(DEFAULT_EXCLUDED_DIRECTORIES),
) -> None:
    root = workspace_root.resolve()

    def ignore(_directory: str, names: list[str]) -> set[str]:
        excluded = excluded_directories.intersection(names)
        return excluded | BENCHMARK_FILES.intersection(names)

    shutil.copytree(root, destination, ignore=ignore, symlinks=True)


def clone_tracked_workspace(
    workspace_root: pathlib.Path,
    destination: pathlib.Path,
    excluded_prefixes: tuple[str, ...] = (".acdd/", "benchmarks/results/"),
) -> int:
    """Copy Git-tracked files from workspace_root into destination without .git or state sidecars."""
    root = workspace_root.resolve()
    probe = subprocess.run(
        ["git", "-C", str(root), "ls-files", "-z"],
        check=False,
        capture_output=True,
    )
    if probe.returncode != 0:
        copy_workspace_snapshot(root, destination)
        return sum(1 for item in destination.rglob("*") if item.is_file())

    destination.mkdir(parents=True, exist_ok=True)
    copied = 0
    for raw_rel in probe.stdout.split(b"\0"):
        if not raw_rel:
            continue
        rel = raw_rel.decode("utf-8", errors="surrogateescape")
        if any(rel.startswith(prefix) for prefix in excluded_prefixes):
            continue
        src = root / rel
        if src.is_symlink() or not src.is_file():
            continue
        dst = destination / rel
        dst.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(src, dst)
        copied += 1
    return copied


def percentile(values: list[float], pct: float) -> float:
    if not values:
        raise ValueError("percentile requires at least one value")
    ordered = sorted(values)
    if len(ordered) == 1:
        return float(ordered[0])
    rank = (pct / 100.0) * (len(ordered) - 1)
    low = math.floor(rank)
    high = math.ceil(rank)
    if low == high:
        return float(ordered[low])
    weight = rank - low
    return float(ordered[low] * (1.0 - weight) + ordered[high] * weight)


def summarize_series(values: list[float], decimals: int = 3) -> dict[str, float]:
    if not values:
        raise ValueError("summarize_series requires at least one value")
    return {
        "median": round(statistics.median(values), decimals),
        "p95": round(percentile(values, 95.0), decimals),
        "mean": round(statistics.fmean(values), decimals),
        "min": round(min(values), decimals),
        "max": round(max(values), decimals),
    }


def database_bytes(db: pathlib.Path) -> int:
    return sum(
        path.stat().st_size
        for path in (db, pathlib.Path(f"{db}-wal"), pathlib.Path(f"{db}-shm"))
        if path.exists()
    )


def run_cli_json(
    binary: pathlib.Path,
    repo: pathlib.Path,
    db: pathlib.Path,
    subcommand: list[str],
    work: pathlib.Path,
    extra_env: dict[str, str] | None = None,
    label: str = "cli",
) -> dict[str, Any]:
    """Execute `<binary> --root <repo> --db <db> --format json <subcommand...>` with wait4 metrics."""
    stdout_path = work / f"{label}.stdout"
    stderr_path = work / f"{label}.stderr"
    command = [
        str(binary),
        "--root",
        str(repo),
        "--db",
        str(db),
        "--profile",
        "code",
        "--format",
        "json",
        *subcommand,
    ]
    environment = os.environ.copy()
    if extra_env:
        environment.update(extra_env)
    host_load_before = list(os.getloadavg()) if hasattr(os, "getloadavg") else None
    started = time.perf_counter()
    with stdout_path.open("wb") as stdout, stderr_path.open("wb") as stderr:
        process = subprocess.Popen(command, cwd=repo, env=environment, stdout=stdout, stderr=stderr)
        usage = None
        if hasattr(os, "wait4"):
            while True:
                try:
                    _, status, usage = os.wait4(process.pid, 0)
                    break
                except InterruptedError:
                    continue
            process.returncode = os.waitstatus_to_exitcode(status)
        else:
            process.wait()
    elapsed = time.perf_counter() - start_time(started)
    stdout_text = stdout_path.read_text(errors="replace")
    stderr_text = stderr_path.read_text(errors="replace")
    if process.returncode:
        detail = stderr_text.strip() or stdout_text.strip()
        raise RuntimeError(f"{subcommand[0]} failed ({process.returncode}) for {binary}: {detail}")
    try:
        payload = json.loads(stdout_text)
    except json.JSONDecodeError as error:
        raise RuntimeError(f"{subcommand[0]} returned invalid JSON: {error}") from error

    metrics: dict[str, Any] = {
        "wall_seconds": elapsed,
        "wall_ms": elapsed * 1000.0,
        "host_load_before": host_load_before,
        "host_load_after": list(os.getloadavg()) if hasattr(os, "getloadavg") else None,
        "payload": payload,
    }
    if usage is not None:
        metrics["user_seconds"] = usage.ru_utime
        metrics["system_seconds"] = usage.ru_stime
        metrics["cpu_seconds"] = usage.ru_utime + usage.ru_stime
        peak_rss = usage.ru_maxrss
        metrics["peak_rss_bytes"] = peak_rss if os.uname().sysname == "Darwin" else peak_rss * 1024
        metrics["peak_rss_mib"] = metrics["peak_rss_bytes"] / (1024.0 * 1024.0)
    return metrics


def start_time(started: float) -> float:
    return started
