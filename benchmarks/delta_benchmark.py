#!/usr/bin/env python3
"""Incremental delta benchmark for ACDD v0.6.2 in an isolated workspace clone."""

from __future__ import annotations

import argparse
import hashlib
import json
import pathlib
import shutil
import statistics
import subprocess
import sys
import tempfile
from typing import Any

from benchmark_profile import (
    clone_tracked_workspace,
    resolve_binary,
    run_cli_json,
    summarize_series,
)


def _select_delta_targets(clone_root: pathlib.Path) -> dict[str, list[str]]:
    search_roots = ("crates", "src", "extensions", "packages", "services")
    for ext, label in ((".rs", "rs"), (".py", "py"), (".ts", "ts")):
        code_files = sorted(
            str(path.relative_to(clone_root))
            for prefix in search_roots
            if (clone_root / prefix).is_dir()
            for path in (clone_root / prefix).rglob(f"*{ext}")
            if path.is_file()
        )
        if len(code_files) >= 10:
            break
    else:
        raise RuntimeError(f"expected at least 10 tracked source files in cloned workspace: {clone_root}")

    md_candidates = [
        path
        for path in ("docs/README.md", "docs/architecture/README.md", "docs/architecture/index.md", "README.md", "ROADMAP.md")
        if (clone_root / path).is_file()
    ]
    if not md_candidates and (clone_root / "docs").is_dir():
        md_candidates = sorted(
            str(path.relative_to(clone_root))
            for path in (clone_root / "docs").rglob("*.md")
            if path.is_file()
        )
    if not md_candidates:
        raise RuntimeError("expected at least 1 .md file in cloned workspace")

    return {
        f"edit_1_{label}": [code_files[0]],
        f"edit_10_{label}": code_files[:10],
        "edit_1_md": [md_candidates[0]],
    }


def _comment_suffix(rel_path: str, tag: str) -> bytes:
    if rel_path.endswith(".md"):
        return f"\n<!-- {tag} -->\n".encode("utf-8")
    if rel_path.endswith(".py"):
        return f"\n# {tag}\n".encode("utf-8")
    return f"\n// {tag}\n".encode("utf-8")


def _rewrite_clean_adapter(clone_root: pathlib.Path) -> None:
    """Ensure acdd.yaml exists cleanly inside the isolated clone with no external side-effects."""
    adapter = clone_root / "acdd.yaml"
    if adapter.is_file():
        return
    adapter.write_text("roots:\n  - src\n  - crates\ndocs:\n  - docs\n")


def benchmark_delta(
    binary: str | pathlib.Path,
    repo: str | pathlib.Path,
    repeats: int = 5,
) -> dict[str, Any]:
    repo_path = pathlib.Path(repo).expanduser().resolve()
    if not repo_path.is_dir() or not (repo_path / "acdd.yaml").is_file():
        raise ValueError(f"workspace must be a directory containing acdd.yaml: {repo_path}")
    if repeats < 1:
        raise ValueError("repeats must be at least 1")

    acdd_bin = resolve_binary(binary)
    with tempfile.TemporaryDirectory(prefix="acdd-delta-bench-") as directory:
        work = pathlib.Path(directory)
        clone_root = work / "workspace"
        copied_files = clone_tracked_workspace(repo_path, clone_root)
        _rewrite_clean_adapter(clone_root)
        targets = _select_delta_targets(clone_root)

        base_db = work / "base-index.sqlite"
        cache_dir = work / "extraction-cache"
        warm_build = run_cli_json(
            binary=acdd_bin,
            repo=clone_root,
            db=base_db,
            subcommand=["build"],
            work=work,
            extra_env={"ACDD_EXTRACTION_CACHE_DIR": str(cache_dir)},
            label="warm-build",
        )

        # Save original bytes for all target files so each repetition starts identically.
        originals = {
            rel: (clone_root / rel).read_bytes()
            for paths in targets.values()
            for rel in paths
        }

        scenarios_report: dict[str, Any] = {}
        for scenario_id, rel_paths in targets.items():
            runs: list[dict[str, Any]] = []
            for rep in range(1, repeats + 1):
                run_work = work / f"{scenario_id}-rep-{rep}"
                run_work.mkdir(parents=True, exist_ok=True)
                run_db = run_work / "index.sqlite"
                shutil.copy2(base_db, run_db)

                try:
                    for rel in rel_paths:
                        target = clone_root / rel
                        suffix = _comment_suffix(rel, f"acdd-delta-bench-{scenario_id}-{rep}")
                        target.write_bytes(originals[rel] + suffix)

                    subcommand = ["delta"]
                    for rel in rel_paths:
                        subcommand.extend(["--modified", rel])

                    timing = run_cli_json(
                        binary=acdd_bin,
                        repo=clone_root,
                        db=run_db,
                        subcommand=subcommand,
                        work=run_work,
                        extra_env={"ACDD_EXTRACTION_CACHE_DIR": str(cache_dir)},
                        label="delta",
                    )
                    payload = timing["payload"]
                    run_entry = {
                        "repetition": rep,
                        "wall_ms": round(timing["wall_ms"], 3),
                        "cpu_seconds": round(timing["cpu_seconds"], 4) if "cpu_seconds" in timing else None,
                        "peak_rss_mib": round(timing["peak_rss_mib"], 3) if "peak_rss_mib" in timing else None,
                        "changed_files": payload.get("changed_files"),
                        "affected_owners": payload.get("affected_owners"),
                        "reparsed_files": payload.get("reparsed_files"),
                        "rewritten_files": payload.get("rewritten_files"),
                        "output_root": payload.get("output_root"),
                        "phases_ms": {
                            key: value
                            for key, value in payload.items()
                            if key.endswith("_ms")
                            and isinstance(value, (int, float))
                            and not isinstance(value, bool)
                        },
                    }
                    runs.append(run_entry)
                    print(
                        f"[delta:{scenario_id}] run {rep}/{repeats}: {run_entry['wall_ms']:.1f}ms "
                        f"changed={run_entry['changed_files']} affected={run_entry['affected_owners']}",
                        file=sys.stderr,
                        flush=True,
                    )
                finally:
                    for rel in rel_paths:
                        (clone_root / rel).write_bytes(originals[rel])

            wall_stats = summarize_series([float(r["wall_ms"]) for r in runs], decimals=3)
            cpu_vals = [float(r["cpu_seconds"]) for r in runs if r.get("cpu_seconds") is not None]
            rss_vals = [float(r["peak_rss_mib"]) for r in runs if r.get("peak_rss_mib") is not None]
            scenarios_report[scenario_id] = {
                "modified_files": rel_paths,
                "modified_count": len(rel_paths),
                "repeats": len(runs),
                "wall_ms": wall_stats,
                "cpu_seconds": summarize_series(cpu_vals, decimals=4) if cpu_vals else None,
                "peak_rss_mib": summarize_series(rss_vals, decimals=3) if rss_vals else None,
                "changed_files": runs[-1]["changed_files"],
                "affected_owners": runs[-1]["affected_owners"],
                "reparsed_files": runs[-1]["reparsed_files"],
                "rewritten_files": runs[-1]["rewritten_files"],
                "runs": runs,
            }

    return {
        "workspace": str(repo_path),
        "binary": str(acdd_bin),
        "binary_sha256": hashlib.sha256(acdd_bin.read_bytes()).hexdigest(),
        "cloned_tracked_files": copied_files,
        "warm_build_wall_ms": round(warm_build["wall_ms"], 3),
        "isolation_policy": "cloned tracked files into TemporaryDirectory; source workspace untouched",
        "scenarios": scenarios_report,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=pathlib.Path, default=pathlib.Path("."), help="workspace containing acdd.yaml")
    parser.add_argument("--binary", required=True, help="ACDD executable or PATH command")
    parser.add_argument("--repeats", type=int, default=5, help="repetitions per delta scenario (default: 5)")
    parser.add_argument("--output", type=pathlib.Path, help="optional JSON report output path")
    args = parser.parse_args()

    try:
        report = benchmark_delta(binary=args.binary, repo=args.repo, repeats=args.repeats)
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
