#!/usr/bin/env python3
"""Orchestrator CLI for ContextUnity ACDD v0.6.2 benchmark suites."""

from __future__ import annotations

import argparse
import datetime
import json
import os
import pathlib
import platform
import shutil
import subprocess
import sys
from typing import Any

from benchmark_profile import load_profile, resolve_binary
from cold_build_benchmark import benchmark_cold
from delta_benchmark import benchmark_delta
from mcp_latency_benchmark import benchmark_mcp


def _check_power_profile(allow_power_saver: bool) -> str | None:
    ctl = shutil.which("powerprofilesctl")
    if ctl is None:
        return None
    probe = subprocess.run([ctl, "get"], check=False, capture_output=True, text=True)
    if probe.returncode != 0:
        return None
    active = probe.stdout.strip()
    if active == "power-saver" and not allow_power_saver:
        raise RuntimeError(
            "host power profile is 'power-saver' (caps CPU frequency and invalidates timings); "
            "run 'powerprofilesctl set performance' or pass --allow-power-saver"
        )
    return active


def _render_markdown(report: dict[str, Any]) -> str:
    lines = [
        f"# ACDD v0.6.2 Benchmark Report ({report['profile_name']})",
        "",
        f"- **Timestamp (UTC)**: `{report['created_at_utc']}`",
        f"- **Workspace**: `{report['workspace']}`",
        f"- **Binary**: `{report['binary']}`",
        f"- **Power Profile**: `{report.get('power_profile') or 'unknown'}`",
        f"- **CPU / Platform**: `{report.get('cpu_count')}` cores — `{report.get('platform')}`",
        f"- **Repeats**: `{report['repeats']}`",
        "",
    ]

    suites = report.get("suites", {})
    cold = suites.get("cold")
    if cold:
        s = cold["summary"] if cold["mode"] == "single" else cold["candidate"]
        c = s["counts"]
        r = s["resolution"]
        lines.extend(
            [
                "## Cold Build",
                "",
                "| Metric | Value |",
                "| --- | ---: |",
                f"| Wall median (ms) | {s['wall_ms']['median']:.2f} |",
                f"| Wall p95 (ms) | {s['wall_ms']['p95']:.2f} |",
                f"| Wall mean (ms) | {s['wall_ms']['mean']:.2f} |",
                f"| CPU median (s) | {s['cpu_seconds']['median']:.3f} |",
                f"| Peak RSS median (MiB) | {s['peak_rss_mib']['median']:.2f} |",
                f"| DB size (MiB) | {s['db_size_mib']:.2f} |",
                f"| Files | {c.get('files')} |",
                f"| Nodes | {c.get('nodes')} |",
                f"| Edges | {c.get('edges')} |",
                f"| Doc sections | {c.get('doc_sections')} |",
                f"| Resolution (%) | {r.get('resolved_percent_excluding_external')}% |",
                f"| Deterministic `output_root` | `{s['deterministic_output_root']}` |",
                "",
            ]
        )

    delta = suites.get("delta")
    if delta:
        lines.extend(
            [
                "## Incremental Delta",
                "",
                f"- Warm baseline build: `{delta['warm_build_wall_ms']:.2f} ms` ({delta['cloned_tracked_files']} tracked files)",
                "",
                "| Scenario | Modified | Affected Owners | Wall median (ms) | Wall p95 (ms) | Wall mean (ms) | CPU median (s) | Peak RSS (MiB) |",
                "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |",
            ]
        )
        for sid, entry in delta["scenarios"].items():
            w = entry["wall_ms"]
            cpu_med = entry["cpu_seconds"]["median"] if entry.get("cpu_seconds") else 0.0
            rss_med = entry["peak_rss_mib"]["median"] if entry.get("peak_rss_mib") else 0.0
            lines.append(
                f"| `{sid}` | {entry['modified_count']} | {entry['affected_owners']} | "
                f"{w['median']:.2f} | {w['p95']:.2f} | {w['mean']:.2f} | {cpu_med:.3f} | {rss_med:.2f} |"
            )
        lines.append("")

    mcp = suites.get("mcp")
    if mcp:
        inotify = mcp.get("inotify_acdd_yaml", {})
        lines.extend(
            [
                "## MCP Latency (`acdd serve`)",
                "",
                f"- Initialize latency: `{mcp['initialize_ms']:.2f} ms`",
                f"- Peak RSS: `{mcp['peak_rss_mib']:.2f} MiB`",
                f"- `inotify` `acdd.yaml` opens: `warm={inotify.get('warm_opens')}`, `initial={inotify.get('initial_opens')}` "
                f"(`single_load_on_warm_call={inotify.get('single_load_on_warm_call')}`)",
                f"- Scenarios: `{mcp['scenario_count']}` (total errors: `{mcp['total_errors']}`)",
                "",
                "| Scenario | Tool | p50 (ms) | p95 (ms) | mean (ms) | Payload (B) | Peak RSS (MiB) |",
                "| --- | --- | ---: | ---: | ---: | ---: | ---: |",
            ]
        )
        for row in mcp["scenarios"]:
            lines.append(
                f"| `{row['id']}` | `{row['tool']}` | {row['p50_ms']:.2f} | {row['p95_ms']:.2f} | "
                f"{row['mean_ms']:.2f} | {row['payload_bytes_median']} | {row['peak_rss_mib']:.2f} |"
            )
        lines.append("")

    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--profile",
        type=pathlib.Path,
        default=pathlib.Path("benchmarks/profiles/commerce-release-update.json"),
        help="path to benchmark JSON profile",
    )
    parser.add_argument("--workspace", type=pathlib.Path, help="override workspace root")
    parser.add_argument("--binary", help="ACDD binary path (defaults to profile setting)")
    parser.add_argument("--baseline-binary", help="optional baseline binary for paired cold comparison")
    parser.add_argument(
        "--suite",
        choices=("all", "cold", "delta", "mcp"),
        default="all",
        help="benchmark suite(s) to execute (default: all)",
    )
    parser.add_argument("--repeats", type=int, default=5, help="repetitions per measurement (default: 5)")
    parser.add_argument("--output", type=pathlib.Path, help="output JSON path (also writes companion .md)")
    parser.add_argument(
        "--allow-power-saver",
        action="store_true",
        help="allow running even if powerprofilesctl reports power-saver",
    )
    args = parser.parse_args()

    if args.repeats < 1:
        parser.error("--repeats must be at least 1")

    try:
        power_profile = _check_power_profile(args.allow_power_saver)
        profile = load_profile(args.profile, args.workspace)
        binary = resolve_binary(args.binary or profile.acdd_binary)
    except (RuntimeError, ValueError) as error:
        parser.error(str(error))

    suites: dict[str, Any] = {}
    if args.suite in ("all", "cold"):
        suites["cold"] = benchmark_cold(
            binary=binary,
            repo=profile.workspace_root,
            repeats=args.repeats,
            baseline_binary=args.baseline_binary,
        )
    if args.suite in ("all", "delta"):
        suites["delta"] = benchmark_delta(
            binary=binary,
            repo=profile.workspace_root,
            repeats=args.repeats,
        )
    if args.suite in ("all", "mcp"):
        suites["mcp"] = benchmark_mcp(
            profile=profile,
            binary=binary,
            repeats=args.repeats,
        )

    report = {
        "schema_version": 1,
        "created_at_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "profile": str(profile.path),
        "profile_name": profile.name,
        "workspace": str(profile.workspace_root),
        "binary": str(binary),
        "power_profile": power_profile,
        "platform": platform.platform(),
        "python": sys.version.split()[0],
        "cpu_count": os.cpu_count(),
        "repeats": args.repeats,
        "suite": args.suite,
        "suites": suites,
    }

    markdown = _render_markdown(report)
    rendered_json = json.dumps(report, indent=2, sort_keys=True)
    if args.output is not None:
        out_json = args.output.expanduser().resolve()
        out_json.parent.mkdir(parents=True, exist_ok=True)
        out_json.write_text(rendered_json + "\n")
        out_md = out_json.with_suffix(".md")
        out_md.write_text(markdown)
        print(f"wrote {out_json} and {out_md}", file=sys.stderr)
    else:
        print(rendered_json)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
