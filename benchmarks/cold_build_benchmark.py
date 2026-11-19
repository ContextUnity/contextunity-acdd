#!/usr/bin/env python3
"""Cold build benchmark for ACDD v0.6.2 (single-binary and optional paired mode)."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import sqlite3
import statistics
import subprocess
import sys
import tempfile
from typing import Any

from benchmark_profile import (
    database_bytes,
    resolve_binary,
    run_cli_json,
    summarize_series,
)
from cold_coverage_metrics import compare_evidence, digest, evidence, metrics


def query_overview(binary: pathlib.Path, repo: pathlib.Path, db: pathlib.Path) -> dict[str, Any]:
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
        "query",
        "overview",
    ]
    result = subprocess.run(command, cwd=repo, check=False, capture_output=True, text=True)
    if result.returncode:
        raise RuntimeError(
            f"query overview failed ({result.returncode}) for {binary}: {result.stderr.strip()}"
        )
    try:
        overview = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise RuntimeError(f"query overview returned invalid JSON for {binary}: {error}") from error

    metadata = {row["key"]: row["value"] for row in overview.get("metadata", [])}
    if pathlib.Path(metadata.get("workspace_root", "")).resolve() != repo:
        raise RuntimeError(
            f"index root mismatch: expected {repo}, got {metadata.get('workspace_root')!r}"
        )
    counts = overview.get("counts")
    languages = overview.get("languages", {}).get("items", [])
    if not isinstance(counts, dict) or not isinstance(languages, list):
        raise RuntimeError("query overview is missing graph counts or language coverage")

    with sqlite3.connect(f"file:{db}?mode=ro", uri=True) as connection:
        persisted_metadata = dict(
            connection.execute(
                "SELECT key,value FROM metadata WHERE key IN ('index_semantics_version','indexer_engine')"
            )
        )
        statuses = dict(
            connection.execute(
                "SELECT status,sum(records) FROM coverage_language_counts GROUP BY status"
            )
        )
        components = [
            row[0]
            for row in connection.execute(
                "SELECT DISTINCT p.path FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id "
                "WHERE n.kind='component' ORDER BY p.path"
            )
        ]

    resolved = int(statuses.get("resolved", 0))
    ambiguous = int(statuses.get("ambiguous", 0))
    unresolved = int(statuses.get("unresolved", 0))
    external = int(statuses.get("external", 0))
    resolvable = resolved + ambiguous + unresolved

    return {
        "workspace_root": metadata["workspace_root"],
        "index_semantics_version": persisted_metadata.get("index_semantics_version"),
        "indexer_engine": persisted_metadata.get("indexer_engine"),
        "compiled_profiles": overview.get("compiled_profiles"),
        "counts": counts,
        "resolution": {
            "resolved": resolved,
            "ambiguous": ambiguous,
            "unresolved": unresolved,
            "external_imports": external,
            "resolved_percent_excluding_external": round(100.0 * resolved / resolvable, 4)
            if resolvable
            else 0.0,
        },
        "components": components,
    }


def domain_commitments(db: pathlib.Path) -> dict[str, str]:
    with sqlite3.connect(f"file:{db}?mode=ro", uri=True) as connection:
        return dict(
            connection.execute(
                "SELECT domain, CASE WHEN typeof(digest)='blob' THEN lower(hex(digest)) ELSE digest END "
                "FROM domain_commitments WHERE owner_id=0 ORDER BY domain"
            )
        )


def root_inputs_digest(db: pathlib.Path) -> str:
    with sqlite3.connect(f"file:{db}?mode=ro", uri=True) as connection:
        metadata = dict(
            connection.execute(
                "SELECT key,value FROM metadata WHERE key IN "
                "('schema_version','indexer_engine','workspace_root','adapter','corpus_hash',"
                "'commitment_algorithm','inventory_snapshot','manifest_digest')"
            )
        )
    if len(metadata) != 8:
        raise RuntimeError("missing commitment root input metadata")
    return hashlib.sha256(json.dumps(metadata, sort_keys=True).encode()).hexdigest()


def run_single_cold_build(
    binary: pathlib.Path,
    repo: pathlib.Path,
    repetition: int,
) -> tuple[dict[str, Any], dict[str, Any]]:
    with tempfile.TemporaryDirectory(prefix="acdd-cold-bench-") as directory:
        work = pathlib.Path(directory)
        db = work / "index.sqlite"
        timing = run_cli_json(
            binary=binary,
            repo=repo,
            db=db,
            subcommand=["build"],
            work=work,
            extra_env={"ACDD_EXTRACTION_CACHE_DIR": str(work / "extraction-cache")},
            label="build",
        )
        if not db.is_file():
            raise RuntimeError(f"build succeeded without creating its database: {db}")

        build = timing["payload"]
        db_bytes = database_bytes(db)
        graph = query_overview(binary, repo, db)
        graph_evidence = evidence(db)
        run = {
            "repetition": repetition,
            "wall_ms": round(timing["wall_ms"], 3),
            "wall_seconds": round(timing["wall_seconds"], 4),
            "cpu_seconds": round(timing["cpu_seconds"], 4) if "cpu_seconds" in timing else None,
            "user_seconds": round(timing["user_seconds"], 4) if "user_seconds" in timing else None,
            "system_seconds": round(timing["system_seconds"], 4) if "system_seconds" in timing else None,
            "peak_rss_bytes": timing.get("peak_rss_bytes"),
            "peak_rss_mib": round(timing["peak_rss_mib"], 3) if "peak_rss_mib" in timing else None,
            "database_bytes": db_bytes,
            "db_size_mib": round(db_bytes / (1024.0 * 1024.0), 3),
            "output_root": build.get("output_root"),
            "domain_commitments": domain_commitments(db),
            "root_inputs_digest": root_inputs_digest(db),
            "phases_ms": {
                key: value
                for key, value in build.items()
                if key.endswith("_ms")
                and isinstance(value, (int, float))
                and not isinstance(value, bool)
            },
            "graph": graph,
            "code_coverage": metrics(graph_evidence, db_bytes, timing["wall_seconds"]),
            "host_load_before": timing.get("host_load_before"),
            "host_load_after": timing.get("host_load_after"),
        }
        if isinstance(build.get("pipeline"), dict):
            run["pipeline"] = build["pipeline"]
        return run, graph_evidence


def summarize_cold_runs(runs: list[dict[str, Any]]) -> dict[str, Any]:
    wall_stats = summarize_series([float(r["wall_ms"]) for r in runs], decimals=3)
    cpu_values = [float(r["cpu_seconds"]) for r in runs if r.get("cpu_seconds") is not None]
    rss_values = [float(r["peak_rss_mib"]) for r in runs if r.get("peak_rss_mib") is not None]
    db_values = [float(r["db_size_mib"]) for r in runs]
    output_roots = [r.get("output_root") for r in runs]
    evidence_hashes = [r["code_coverage"]["evidence_sha256"] for r in runs]
    commitments_hashes = [digest(r["domain_commitments"]) for r in runs]
    deterministic = (
        len(set(output_roots)) == 1
        and output_roots[0] is not None
        and len(set(evidence_hashes)) == 1
        and len(set(commitments_hashes)) == 1
    )
    phase_medians: dict[str, float] = {}
    phase_keys = sorted({key for r in runs for key in r.get("phases_ms", {})})
    for key in phase_keys:
        vals = [float(r["phases_ms"][key]) for r in runs if key in r["phases_ms"]]
        if vals:
            phase_medians[key] = round(statistics.median(vals), 3)

    return {
        "repeats": len(runs),
        "wall_ms": wall_stats,
        "cpu_seconds": summarize_series(cpu_values, decimals=4) if cpu_values else None,
        "peak_rss_mib": summarize_series(rss_values, decimals=3) if rss_values else None,
        "db_size_mib": round(statistics.median(db_values), 3),
        "database_bytes": int(statistics.median(r["database_bytes"] for r in runs)),
        "counts": runs[-1]["graph"]["counts"],
        "resolution": runs[-1]["graph"]["resolution"],
        "output_root": output_roots[0],
        "deterministic_output_root": deterministic,
        "phases_median_ms": phase_medians,
        "runs": runs,
    }


def compare_cold(baseline: dict[str, Any], candidate: dict[str, Any]) -> dict[str, Any]:
    before_wall = baseline["wall_ms"]["median"]
    after_wall = candidate["wall_ms"]["median"]
    before_counts = baseline["counts"]
    after_counts = candidate["counts"]
    count_keys = ("files", "nodes", "edges", "doc_sections", "parse_errors", "unresolved", "external_imports")
    paired_changes = [
        100.0 * (new["wall_ms"] / old["wall_ms"] - 1.0)
        for old, new in zip(baseline["runs"], candidate["runs"], strict=True)
        if old["wall_ms"]
    ]
    return {
        "candidate_wall_change_percent": round(100.0 * (after_wall / before_wall - 1.0), 3)
        if before_wall
        else None,
        "median_paired_wall_change_percent": round(statistics.median(paired_changes), 3)
        if paired_changes
        else None,
        "resolved_coverage_change_percentage_points": round(
            candidate["resolution"]["resolved_percent_excluding_external"]
            - baseline["resolution"]["resolved_percent_excluding_external"],
            4,
        ),
        "graph_count_deltas": {
            key: int(after_counts.get(key, 0)) - int(before_counts.get(key, 0))
            for key in count_keys
        },
        "output_roots_match": (
            baseline["output_root"] == candidate["output_root"]
            and baseline["output_root"] is not None
        ),
    }


def benchmark_cold(
    binary: str | pathlib.Path,
    repo: str | pathlib.Path,
    repeats: int = 5,
    baseline_binary: str | pathlib.Path | None = None,
    max_peak_rss_mb: float | None = None,
    comparison_policy: str = "strict",
) -> dict[str, Any]:
    repo_path = pathlib.Path(repo).expanduser().resolve()
    if not repo_path.is_dir() or not (repo_path / "acdd.yaml").is_file():
        raise ValueError(f"workspace must be a directory containing acdd.yaml: {repo_path}")
    if repeats < 1:
        raise ValueError("repeats must be at least 1")

    candidate_bin = resolve_binary(binary)
    baseline_bin = resolve_binary(baseline_binary) if baseline_binary else None

    if baseline_bin is None:
        runs: list[dict[str, Any]] = []
        for idx in range(repeats):
            run, _ = run_single_cold_build(candidate_bin, repo_path, idx + 1)
            runs.append(run)
            print(
                f"[cold] run {idx + 1}/{repeats}: {run['wall_ms']:.1f}ms "
                f"rss={run['peak_rss_mib']}MiB db={run['db_size_mib']}MiB",
                file=sys.stderr,
                flush=True,
            )
        summary = summarize_cold_runs(runs)
        report: dict[str, Any] = {
            "mode": "single",
            "workspace": str(repo_path),
            "binary": str(candidate_bin),
            "binary_sha256": hashlib.sha256(candidate_bin.read_bytes()).hexdigest(),
            "database_policy": "fresh temporary database and extraction cache per run; removed on completion",
            "os_page_cache_policy": "not flushed; only index and extraction caches are cold",
            "summary": summary,
            "passed": summary["deterministic_output_root"],
        }
    else:
        results: dict[str, list[dict[str, Any]]] = {"baseline": [], "candidate": []}
        binaries = {"baseline": baseline_bin, "candidate": candidate_bin}
        first_evidence: dict[str, dict[str, Any]] = {}
        for idx in range(repeats):
            order = ("baseline", "candidate") if idx % 2 == 0 else ("candidate", "baseline")
            for label in order:
                run, graph_ev = run_single_cold_build(binaries[label], repo_path, idx + 1)
                results[label].append(run)
                if label not in first_evidence:
                    first_evidence[label] = graph_ev
                print(
                    f"[cold:{label}] run {idx + 1}/{repeats}: {run['wall_ms']:.1f}ms",
                    file=sys.stderr,
                    flush=True,
                )
        baseline_sum = summarize_cold_runs(results["baseline"])
        candidate_sum = summarize_cold_runs(results["candidate"])
        ev_diff = compare_evidence(first_evidence["baseline"], first_evidence["candidate"])
        code_coverage_preserved = not any(
            ev_diff[key]
            for key in (
                "lost_code_files",
                "lost_code_nodes",
                "lost_exact_call_pairs",
                "lost_exact_call_occurrences",
                "lost_code_edges",
                "lost_resolved_references",
            )
        ) and not ev_diff["aligned_test_reachability"]["lost_production_nodes"]
        comp = compare_cold(baseline_sum, candidate_sum)
        semantic_parity = (
            all(delta == 0 for delta in comp["graph_count_deltas"].values())
            and comp["output_roots_match"]
        )
        passed = (
            semantic_parity
            if comparison_policy == "strict"
            else (
                code_coverage_preserved
                and baseline_sum["deterministic_output_root"]
                and candidate_sum["deterministic_output_root"]
            )
        )
        report = {
            "mode": "paired",
            "workspace": str(repo_path),
            "baseline_binary": str(baseline_bin),
            "candidate_binary": str(candidate_bin),
            "binary_sha256": {
                "baseline": hashlib.sha256(baseline_bin.read_bytes()).hexdigest(),
                "candidate": hashlib.sha256(candidate_bin.read_bytes()).hexdigest(),
            },
            "comparison_policy": comparison_policy,
            "database_policy": "fresh temporary database and extraction cache per run; removed on completion",
            "os_page_cache_policy": "not flushed; only index and extraction caches are cold",
            "baseline": baseline_sum,
            "candidate": candidate_sum,
            "comparison": comp,
            "code_coverage_preserved": code_coverage_preserved,
            "semantic_parity": semantic_parity,
            "passed": passed,
        }

    if max_peak_rss_mb is not None:
        target_runs = report["summary"]["runs"] if baseline_bin is None else report["candidate"]["runs"]
        limit_bytes = max_peak_rss_mb * 1_000_000
        rss_ok = all(
            run.get("peak_rss_bytes") is not None and run["peak_rss_bytes"] < limit_bytes
            for run in target_runs
        )
        report["peak_rss_limit_bytes"] = limit_bytes
        report["peak_rss_limit_passed"] = rss_ok
        report["passed"] = report["passed"] and rss_ok

    return report


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=pathlib.Path, default=pathlib.Path("."), help="workspace containing acdd.yaml")
    parser.add_argument("--binary", required=True, help="ACDD executable or PATH command")
    parser.add_argument("--baseline-binary", help="optional baseline ACDD executable for paired comparison")
    parser.add_argument("--repeats", type=int, default=5, help="cold builds per binary (default: 5)")
    parser.add_argument("--max-peak-rss-mb", type=float, help="optional RSS limit in decimal MB")
    parser.add_argument("--comparison-policy", choices=("strict", "coverage"), default="strict")
    parser.add_argument("--output", type=pathlib.Path, help="optional JSON report output path")
    args = parser.parse_args()

    try:
        report = benchmark_cold(
            binary=args.binary,
            repo=args.repo,
            repeats=args.repeats,
            baseline_binary=args.baseline_binary,
            max_peak_rss_mb=args.max_peak_rss_mb,
            comparison_policy=args.comparison_policy,
        )
    except (OSError, RuntimeError, ValueError) as error:
        parser.error(str(error))

    rendered = json.dumps(report, indent=2, sort_keys=True)
    if args.output is not None:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(rendered + "\n")
    print(rendered)
    return 0 if report.get("passed", False) else 1


if __name__ == "__main__":
    raise SystemExit(main())
