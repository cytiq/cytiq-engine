#!/usr/bin/env python3
"""Events export benchmark: the wide `cytiq.events.v1` layout against the list
layout, and the wide layout's codecs and row groups.

Each export runs as its own process, measured with wait4 (wall, CPU, peak
RSS). SQL projection and sequential scans run in DuckDB, and column reads in
PyArrow, both from the pinned lakehouse environment
(`parity/lakehouse/requirements.lock`). The decision rule is fixed here,
before any run (see `decide`).
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import shutil
import statistics
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))
from run import ROOT, host_metadata, measure, sha256  # noqa: E402

WORKLOADS = {
    "f32_1m": {"events": 1_000_000, "channels": 30, "types": "float32"},
    "f32_10m": {"events": 10_000_000, "channels": 30, "types": "float32"},
    "mixed_10m": {"events": 10_000_000, "channels": 30, "types": "mixed"},
    # Spectral and mass panels: memory per channel, not per event.
    "f32x200_1m": {"events": 1_000_000, "channels": 200, "types": "float32"},
}
SMOKE_WORKLOADS = {
    "f32_smoke": {"events": 20_000, "channels": 30, "types": "float32"},
    "mixed_smoke": {"events": 20_000, "channels": 8, "types": "mixed"},
}
# Position of the channel every projection reads (`C7` in the synthetic
# workloads), counted in `$Pn` order without the keys.
PROJECTED = 6


def wide(name, level=None, codec="zstd", row_group=262_144):
    args = ["--compression", codec, "--row-group-size", str(row_group)]
    if level is not None:
        args += ["--compression-level", str(level)]
    return {"name": name, "layout": "wide", "args": args}


VARIANTS = [
    wide("wide-zstd1-rg256k", 1, row_group=262_144),
    wide("wide-zstd3-rg256k", 3),
    wide("wide-zstd6-rg256k", 6),
    wide("wide-snappy-rg256k", codec="snappy"),
    wide("wide-zstd1-rg128k", 1, row_group=131_072),
    wide("wide-zstd1-rg1m", 1, row_group=1_048_576),
    {"name": "list-zstd1-rg256k", "layout": "list", "row_group": 262_144, "level": 1},
]
REFERENCE = "wide-zstd1-rg256k"
LIST_FACTOR = 1.5
WITHIN = 1.10


def generate(example, path, workload):
    meta = path.with_suffix(".json")
    if path.exists() and meta.exists():
        return json.loads(meta.read_text())
    path.parent.mkdir(parents=True, exist_ok=True)
    output = subprocess.check_output([
        str(example), "generate", "--output", str(path),
        "--events", str(workload["events"]), "--channels", str(workload["channels"]),
        "--types", workload["types"],
    ], text=True)
    info = json.loads(output)
    info["sha256"] = sha256(path)
    meta.write_text(json.dumps(info, indent=2) + "\n")
    return info


def real_workload(path):
    """DATA bytes of a real FCS file, from its HEADER or TEXT offsets."""
    with Path(path).open("rb") as handle:
        header = handle.read(58).decode("ascii")
        start, end = int(header[26:34] or 0), int(header[34:42] or 0)
        if start == 0 or end == 0:
            text_start, text_end = int(header[10:18]), int(header[18:26])
            handle.seek(text_start)
            text = handle.read(text_end - text_start + 1).decode("latin-1")
            delimiter = text[0]
            fields = text[1:].split(delimiter)
            pairs = dict(zip(fields[::2], fields[1::2]))
            start, end = int(pairs["$BEGINDATA"]), int(pairs["$ENDDATA"])
    return {"data_bytes": end - start + 1, "sha256": sha256(path)}


def outputs_of(variant, output):
    if variant["layout"] == "list":
        return [output, Path(f"{output}.channels.parquet")]
    return [output]


def remove(paths):
    for path in paths:
        if path.is_dir():
            shutil.rmtree(path)
        elif path.exists():
            path.unlink()
        record = Path(f"{path}.provenance.json")
        if record.exists():
            record.unlink()


def export_command(variant, binaries, source, output):
    if variant["layout"] == "list":
        return [str(binaries["example"]), "write-list", "--input", str(source),
                "--output", str(output), "--row-group-size", str(variant["row_group"]),
                "--zstd-level", str(variant["level"])]
    return [str(binaries["cytiq"]), "export", "events", "--input", str(source),
            "--output", str(output), "--layer", "raw", *variant["args"]]


def timed(function, repeats):
    """Median wall seconds of `repeats` calls after one warm-up, and the
    last result."""
    result = function()
    times = []
    for _ in range(repeats):
        started = time.perf_counter()
        result = function()
        times.append(time.perf_counter() - started)
    return statistics.median(times), result


def list_position(output):
    """The list column and 1-based position of the projected channel."""
    import duckdb
    rows = duckdb.sql(
        f"SELECT list, position FROM read_parquet('{output}.channels.parquet') "
        f"ORDER BY index").fetchall()
    return rows[min(PROJECTED, len(rows) - 1)]


def channel_columns(output):
    import pyarrow.parquet as pq
    names = pq.read_schema(output).names
    return [name for name in names if name not in
            ("acquisition_id", "layer_id", "source_event_id")]


def projected_column(output):
    columns = channel_columns(output)
    return columns[min(PROJECTED, len(columns) - 1)]


def projection_sql(variant, output):
    if variant["layout"] == "list":
        column, position = list_position(output)
        return f"SELECT avg({column}[{position}]) FROM read_parquet('{output}')"
    name = projected_column(output).replace('"', '""')
    return f'SELECT avg("{name}") FROM read_parquet(\'{output}\')'


def scan_sql(variant, output):
    """Sum of every channel value, column by column or list by list."""
    columns = channel_columns(output)
    if variant["layout"] == "list":
        sums = ", ".join(f"sum(list_sum({c}::DOUBLE[]))" for c in columns)
    else:
        quoted = [c.replace('"', '""') for c in columns]
        sums = ", ".join(f'sum("{c}"::DOUBLE)' for c in quoted)
    return f"SELECT count(*), {sums} FROM read_parquet('{output}')"


def query(variant, output, repeats):
    import duckdb
    import pyarrow.parquet as pq

    connection = duckdb.connect()
    projection, projected = timed(
        lambda: connection.execute(projection_sql(variant, output)).fetchone(), repeats)
    scan, scanned = timed(lambda: connection.execute(scan_sql(variant, output)).fetchone(),
                          repeats)
    if variant["layout"] == "list":
        column, _ = list_position(output)
    else:
        column = projected_column(output)
    arrow_projection, _ = timed(lambda: pq.read_table(output, columns=[column]), repeats)
    arrow_read, table = timed(lambda: pq.read_table(output), repeats)
    return {
        "duckdb_projection_seconds": projection, "duckdb_scan_seconds": scan,
        "pyarrow_projection_seconds": arrow_projection, "pyarrow_read_seconds": arrow_read,
        "projected_mean": projected[0], "scan_count": scanned[0],
        "scan_total": sum(value for value in scanned[1:] if value is not None),
        "rows": table.num_rows,
    }


def run_variant(variant, binaries, source, info, work, repeats, query_repeats):
    output = work / f"{variant['name']}.parquet"
    measurements = []
    for repeat in range(repeats):
        remove(outputs_of(variant, output))
        measurements.append(measure(export_command(variant, binaries, source, output),
                                    work / "logs" / f"{variant['name']}-{repeat}",
                                    dict(os.environ)))
    medians = {key: statistics.median(m[key] for m in measurements)
               for key in measurements[0]}
    size = sum(path.stat().st_size for path in outputs_of(variant, output))
    result = {
        "variant": variant["name"], "layout": variant["layout"], "export": medians,
        "bytes": size, "compression_ratio": info["data_bytes"] / size,
        "events_per_second": info.get("events", 0) / medians["wall_seconds"],
        "data_mb_per_second": info["data_bytes"] / 1e6 / medians["wall_seconds"],
    }
    result.update(query(variant, output, query_repeats))
    if variant["layout"] == "wide":
        verify = measure([str(binaries["example"]), "verify", "--input", str(output)],
                         work / "logs" / f"{variant['name']}-verify", dict(os.environ))
        stdout = (work / "logs" / f"{variant['name']}-verify.stdout").read_text()
        result["verify"] = verify
        result["values_sha256"] = json.loads(stdout)["values_sha256"]
    # Ten-million-event outputs take gigabytes; keep only the measurements.
    remove(outputs_of(variant, output))
    return result


def check_agreement(results):
    """Every wide variant holds the same values; every layout the same sums."""
    digests = {r["values_sha256"] for r in results if r["layout"] == "wide"}
    if len(digests) != 1:
        raise RuntimeError(f"wide variants disagree on values: {digests}")
    reference = results[0]
    for result in results[1:]:
        for key in ("projected_mean", "scan_total"):
            a, b = reference[key], result[key]
            if not abs(a - b) <= 1e-9 * max(abs(a), abs(b), 1.0):
                raise RuntimeError(f"{result['variant']} {key} {b} != {a}")
        if result["rows"] != reference["rows"] or result["scan_count"] != reference["rows"]:
            raise RuntimeError(f"{result['variant']} lost rows")


def decide(results):
    """The rule fixed before measuring.

    - Layout: wide stays canonical unless the list layout is at least
      LIST_FACTOR times better on DuckDB projection or on file size; then a
      human decides, because the bounded reader would need nested types.
    - Row group and zstd level: among the wide zstd variants, the smallest
      row group (then lowest level) whose DuckDB projection and scan are
      within WITHIN of the best; ties go to the lower peak RSS. When no
      variant is within WITHIN on both (the fastest projection and the
      fastest scan are different variants), the eligible ones are those whose
      sum of both times relative to their bests is within WITHIN of the
      smallest sum.
    """
    by_name = {r["variant"]: r for r in results}
    reference, listed = by_name[REFERENCE], by_name.get("list-zstd1-rg256k")
    layout = {"canonical": "wide", "list_wins": False}
    if listed:
        projection = reference["duckdb_projection_seconds"] / listed["duckdb_projection_seconds"]
        size = reference["bytes"] / listed["bytes"]
        layout.update(list_projection_speedup=projection, list_size_gain=size,
                      list_wins=projection >= LIST_FACTOR or size >= LIST_FACTOR)
        if layout["list_wins"]:
            layout["canonical"] = "undecided: bring the numbers back"
    candidates = [r for r in results if r["layout"] == "wide" and "zstd" in r["variant"]]
    best_projection = min(r["duckdb_projection_seconds"] for r in candidates)
    best_scan = min(r["duckdb_scan_seconds"] for r in candidates)
    eligible = [r for r in candidates
                if r["duckdb_projection_seconds"] <= best_projection * WITHIN
                and r["duckdb_scan_seconds"] <= best_scan * WITHIN]
    rule = "projection and scan within 10%"
    if not eligible:
        def score(r):
            return (r["duckdb_projection_seconds"] / best_projection
                    + r["duckdb_scan_seconds"] / best_scan)
        best = min(score(r) for r in candidates)
        eligible = [r for r in candidates if score(r) <= best * WITHIN]
        rule = "combined projection and scan within 10%"

    def key(r):
        group = int(r["variant"].rsplit("rg", 1)[1].replace("k", "000").replace("m", "000000"))
        level = int(r["variant"].split("zstd", 1)[1].split("-", 1)[0])
        return (group, level, r["export"]["peak_rss_bytes"])

    chosen = min(eligible, key=key)
    return {"layout": layout, "settings": chosen["variant"], "rule": rule}


def table(rows):
    header = ("| variant | MB | ratio | export s | Mevents/s | RSS MiB | verify s | "
              "SQL col ms | SQL scan ms | Arrow col ms | Arrow all ms |")
    lines = [header, "|" + "---|" * 11]
    for r in rows:
        verify = r.get("verify", {}).get("wall_seconds")
        lines.append(
            f"| {r['variant']} | {r['bytes'] / 1e6:.1f} | {r['compression_ratio']:.2f} | "
            f"{r['export']['wall_seconds']:.2f} | {r['events_per_second'] / 1e6:.2f} | "
            f"{r['export']['peak_rss_bytes'] / 2**20:.0f} | "
            f"{'' if verify is None else f'{verify:.2f}'} | "
            f"{r['duckdb_projection_seconds'] * 1e3:.1f} | {r['duckdb_scan_seconds'] * 1e3:.1f} | "
            f"{r['pyarrow_projection_seconds'] * 1e3:.1f} | {r['pyarrow_read_seconds'] * 1e3:.1f} |")
    return "\n".join(lines)


def parse_args(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cytiq", type=Path, default=ROOT / "target/release/cytiq")
    parser.add_argument("--example", type=Path,
                        default=ROOT / "target/release/examples/events_layouts")
    parser.add_argument("--work", type=Path, default=ROOT / "target/performance/events")
    parser.add_argument("--workload", action="append", default=[],
                        help="a synthetic workload by name (repeatable)")
    parser.add_argument("--real", action="append", type=Path, default=[],
                        help="a real FCS file to benchmark as well (repeatable)")
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--query-repeats", type=int, default=5)
    parser.add_argument("--smoke", action="store_true",
                        help="small inputs and one repeat; checks the runner, decides nothing")
    return parser.parse_args(argv)


def main(argv=None):
    args = parse_args(argv)
    workloads = SMOKE_WORKLOADS if args.smoke else WORKLOADS
    names = args.workload or list(workloads)
    repeats, query_repeats = (1, 1) if args.smoke else (args.repeats, args.query_repeats)
    binaries = {"cytiq": args.cytiq, "example": args.example}
    for binary in binaries.values():
        if not binary.exists():
            raise SystemExit(f"{binary} is missing; see performance/README.md")
    report = {"host": host_metadata(), "smoke": args.smoke, "workloads": {}}
    summary = ["# Events export benchmark", ""]
    sources = [(name, workloads[name]) for name in names]
    sources += [("real_" + "".join(c if c.isalnum() else "_" for c in path.stem)[:40],
                 {"path": path}) for path in args.real]
    for name, workload in sources:
        work = args.work / name
        work.mkdir(parents=True, exist_ok=True)
        if "path" in workload:
            source, info = workload["path"], real_workload(workload["path"])
        else:
            source = args.work / "inputs" / f"{name}.fcs"
            info = generate(args.example, source, workload)
        print(f"{name}: {info}", flush=True)
        results = []
        for variant in VARIANTS:
            result = run_variant(variant, binaries, source, info, work, repeats, query_repeats)
            if not info.get("events"):
                result["events_per_second"] = result["rows"] / result["export"]["wall_seconds"]
            print(f"  {variant['name']}: {result['bytes']} B, "
                  f"{result['export']['wall_seconds']:.2f} s", flush=True)
            results.append(result)
        # Save the measurements before judging them, and after each workload.
        report["workloads"][name] = {"input": info, "results": results}
        save(args.work, report, summary)
        check_agreement(results)
        decision = decide(results)
        report["workloads"][name]["decision"] = decision
        summary += [f"## {name}", "", f"Input: {json.dumps(info)}", "", table(results), "",
                    f"Decision: {json.dumps(decision)}", ""]
        save(args.work, report, summary)
    print("\n".join(summary))


def save(work, report, summary):
    work.mkdir(parents=True, exist_ok=True)
    (work / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    (work / "summary.md").write_text("\n".join(summary) + "\n")


if __name__ == "__main__":
    main()
