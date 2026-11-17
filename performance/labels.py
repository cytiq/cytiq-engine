#!/usr/bin/env python3
"""Label table layout benchmark: the wide `cytiq.events.labels.v1` layout (one
row per event, one column per label) against a long one (one row per event
and label), with the views a user would write over the wide layout.

Tables are written by PyArrow with cytiq's settings (zstd level 1, Parquet
dictionaries on string columns only, 128k-event row groups) from a
deterministic pattern; the writer is not measured, reading is. Queries run in
DuckDB from the pinned lakehouse environment
(`parity/lakehouse/requirements.lock`), median of five after a warm-up.

The decision rule is fixed here, before any run (see `decide`):
1. A layout whose largest table the bounded verifier could not read as one
   table (the planner's decoded estimate above 7/8 of 8 GiB) is ineligible.
2. Wide stays canonical unless long is at least 1.5x better on per-label
   counts, the label-filtered selection or file size; then the numbers go
   back for review.
3. The UNPIVOT view over the wide layout must answer per-label counts
   within 2x of a materialized long table; otherwise the documentation gives
   a `COPY` recipe that materializes it.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import statistics
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
DECODED_LIMIT = (8 << 30) - (8 << 30) // 8
KEYS_DECODED = 2 * (64 + 4) + 8 + 4 + 12
ACQUISITION = "a" * 64
ANNOTATION = "b" * 64

WORKLOADS = {
    "gates_1m": {"events": 1_000_000, "labels": 32, "kind": "gate"},
    "gates_10m": {"events": 10_000_000, "labels": 32, "kind": "gate"},
    "qc_10m": {"events": 10_000_000, "labels": 5, "kind": "qc"},
    "classifier_1m": {"events": 1_000_000, "labels": 20, "kind": "classifier"},
}
SMOKE_WORKLOADS = {
    "gates_smoke": {"events": 20_000, "labels": 8, "kind": "gate"},
    "classifier_smoke": {"events": 20_000, "labels": 4, "kind": "classifier"},
}


def label_names(workload):
    prefix = "p_c" if workload["kind"] == "classifier" else "g"
    return [f"{prefix}{index}" for index in range(workload["labels"])]


def decoded_row(workload, layout):
    """The planner's decoded bytes of one row (`labels/write.rs`)."""
    value = 5 if workload["kind"] == "classifier" else 1
    if layout == "wide":
        return KEYS_DECODED + workload["labels"] * value
    return KEYS_DECODED + (4 + 8) + value


def columns(workload, ids):
    """State and label cells of `ids`: about one event in sixteen (hashed)
    QC-rejected with null cells, each label a hashed pattern, children
    inside label 0."""
    import pyarrow as pa
    import pyarrow.compute as pc

    def u64(value):
        return pa.scalar(value, pa.uint64())

    rejected = pc.equal(pc.bit_wise_and(pc.multiply(ids, u64(0x9E3779B97F4A7C15)), u64(15)),
                        u64(0))
    state = pc.if_else(rejected, pa.scalar("qc_rejected"),
                       pa.scalar("classified" if workload["kind"] == "classifier"
                                 else "assessed"))
    cells = []
    for index in range(workload["labels"]):
        mixed = pc.bit_wise_and(
            pc.shift_right(pc.multiply(ids, u64(0xBF58476D1CE4E5B9 + 2 * index)), u64(20)),
            u64(1023))
        if workload["kind"] == "classifier":
            value = pc.divide(pc.cast(mixed, pa.float32()), pa.scalar(1023.0, pa.float32()))
        else:
            value = pc.less(mixed, u64(1023 - 25 * index))
            if index and workload["kind"] == "gate":
                value = pc.and_(value, cells[0])
        cells.append(pc.if_else(rejected, pa.scalar(None, value.type), value))
    return state, cells


def write(directory, name, workload):
    import pyarrow as pa
    import pyarrow.parquet as pq

    names = label_names(workload)
    events = workload["events"]
    wide_path = directory / f"{name}.wide.parquet"
    long_path = directory / f"{name}.long.parquet"
    if wide_path.exists() and long_path.exists():
        return wide_path, long_path
    options = {"compression": "zstd", "compression_level": 1,
               "use_dictionary": ["acquisition_id", "annotation_id", "state", "label"],
               "write_statistics": True}
    chunk = 1 << 20
    wide_writer = long_writer = None
    for start in range(0, events, chunk):
        count = min(chunk, events - start)
        ids = pa.array(range(start, start + count), type=pa.uint64())
        state, cells = columns(workload, ids)
        keys = {"acquisition_id": pa.repeat(ACQUISITION, count),
                "annotation_id": pa.repeat(ANNOTATION, count),
                "source_event_id": ids, "state": state}
        wide = pa.table({**keys, **dict(zip(names, cells))})
        if wide_writer is None:
            wide_writer = pq.ParquetWriter(wide_path, wide.schema, **options)
        wide_writer.write_table(wide, row_group_size=131_072)
        for label, cell in zip(names, cells):
            value = "probability" if workload["kind"] == "classifier" else "member"
            table = pa.table({**keys, "label": pa.repeat(label, count), value: cell})
            if long_writer is None:
                long_writer = pq.ParquetWriter(long_path, table.schema, **options)
            long_writer.write_table(table, row_group_size=131_072)
    wide_writer.close()
    long_writer.close()
    return wide_path, long_path


def same(one, two, workload):
    """Equal answers; averages of floats may differ in the last bits with
    DuckDB's parallel summation order."""
    if workload["kind"] != "classifier":
        return one == two
    return len(one) == len(two) and all(
        abs(a - b) <= 1e-9 * max(abs(a), abs(b), 1.0) if isinstance(a, float) else a == b
        for a, b in zip(one, two))


def timed(connection, sql, repeats):
    connection.execute(sql).fetchall()
    times = []
    for _ in range(repeats):
        start = time.perf_counter()
        result = connection.execute(sql).fetchall()
        times.append(time.perf_counter() - start)
    return statistics.median(times), result


def queries(workload, wide, long_):
    names = label_names(workload)
    quoted = ", ".join(f'"{name}"' for name in names)
    target = names[min(7, len(names) - 1)]
    if workload["kind"] == "classifier":
        return {
            "per_label": (
                "SELECT " + ", ".join(f'avg("{n}")' for n in names) + f" FROM '{wide}'",
                f"SELECT label, avg(probability) FROM '{long_}' GROUP BY label ORDER BY label",
                f"SELECT label, avg(probability) FROM (SELECT * FROM '{wide}' UNPIVOT INCLUDE "
                f"NULLS (probability FOR label IN ({quoted}))) GROUP BY label ORDER BY label"),
            "selection": (
                f"SELECT count(*) FROM '{wide}' WHERE \"{target}\" > 0.5",
                f"SELECT count(*) FROM '{long_}' WHERE label = '{target}' AND probability > 0.5",
                None),
        }
    return {
        "per_label": (
            "SELECT " + ", ".join(f'count(*) FILTER (WHERE "{n}")' for n in names)
            + f" FROM '{wide}'",
            f"SELECT label, count(*) FILTER (WHERE member) FROM '{long_}' GROUP BY label "
            "ORDER BY label",
            f"SELECT label, count(*) FILTER (WHERE member) FROM (SELECT * FROM '{wide}' UNPIVOT "
            f"INCLUDE NULLS (member FOR label IN ({quoted}))) GROUP BY label ORDER BY label"),
        "selection": (
            f"SELECT count(*) FROM '{wide}' WHERE \"{target}\"",
            f"SELECT count(*) FROM '{long_}' WHERE label = '{target}' AND member",
            None),
    }


def measure(directory, name, workload, repeats):
    import duckdb

    wide, long_ = write(directory, name, workload)
    connection = duckdb.connect()
    connection.execute("SET autoinstall_known_extensions = false")
    result = {"workload": workload,
              "bytes": {"wide": wide.stat().st_size, "long": long_.stat().st_size},
              "rows": {"wide": workload["events"],
                       "long": workload["events"] * workload["labels"]}}
    result["decoded"] = {layout: result["rows"][layout] * decoded_row(workload, layout)
                         for layout in ("wide", "long")}
    for query, (on_wide, on_long, view) in queries(workload, wide, long_).items():
        wide_time, wide_answer = timed(connection, on_wide, repeats)
        long_time, long_answer = timed(connection, on_long, repeats)
        entry = {"wide": wide_time, "long": long_time}
        if query == "per_label":
            # Long answers are per label name; compare them in column order.
            by_label = dict(long_answer)
            ordered = [by_label.get(name) for name in label_names(workload)]
            entry["same_answer"] = same(list(wide_answer[0]), ordered, workload)
        else:
            entry["same_answer"] = wide_answer == long_answer
        if view:
            entry["unpivot_view"], view_answer = timed(connection, view, repeats)
            entry["same_answer"] = entry["same_answer"] and same(
                [v for _, v in view_answer], [v for _, v in long_answer], workload)
        result[query] = entry
    return result


def decide(results):
    """The rule of the module docstring, per workload."""
    decisions = {}
    for name, result in results.items():
        eligible = {layout: result["decoded"][layout] <= DECODED_LIMIT
                    for layout in ("wide", "long")}
        better = [metric for metric, (wide, long_) in {
            "per_label": (result["per_label"]["wide"], result["per_label"]["long"]),
            "selection": (result["selection"]["wide"], result["selection"]["long"]),
            "bytes": (result["bytes"]["wide"], result["bytes"]["long"]),
        }.items() if long_ * 1.5 <= wide]
        view = result["per_label"].get("unpivot_view")
        decisions[name] = {
            "verifier_reads": eligible,
            "layout": "review" if eligible["long"] and better else "wide",
            "long_better_on": better,
            "view_within_2x": view is not None and view <= 2 * result["per_label"]["long"],
            "answers_agree": all(result[q]["same_answer"] for q in ("per_label", "selection")),
        }
    return decisions


def run(args):
    import duckdb
    import pyarrow

    workloads = SMOKE_WORKLOADS if args.smoke else WORKLOADS
    if args.workload:
        workloads = {args.workload: {**WORKLOADS, **SMOKE_WORKLOADS}[args.workload]}
    args.work.mkdir(parents=True, exist_ok=True)
    results = {}
    for name, workload in workloads.items():
        results[name] = measure(args.work, name, workload, args.repeats)
        print(json.dumps({name: results[name]}, indent=2), flush=True)
    report = {"duckdb": duckdb.__version__, "pyarrow": pyarrow.__version__,
              "results": results, "decisions": decide(results)}
    (args.work / "labels.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report["decisions"], indent=2))
    return 0 if all(d["answers_agree"] for d in report["decisions"].values()) else 1


def parse_args(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--work", type=Path, default=ROOT / "target/performance/labels")
    parser.add_argument("--workload", choices=sorted({**WORKLOADS, **SMOKE_WORKLOADS}))
    parser.add_argument("--smoke", action="store_true")
    parser.add_argument("--repeats", type=int, default=5)
    return parser.parse_args(argv)


if __name__ == "__main__":
    sys.exit(run(parse_args()))
