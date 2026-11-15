#!/usr/bin/env python3
"""Lakehouse interop check: `cytiq export events` tables read by PyArrow and
DuckDB hold exactly the FCS values.

Every case exports with the release binary, then checks:
- PyArrow: each channel's values are the FCS DATA bytes, bit for bit (NaN
  payloads, -0.0, infinities, subnormals, integers above 2^63), and the
  keys are the acquisition and `0..n` in order;
- DuckDB: column types, counts, id ranges, quoted unusual names, the channel
  dictionary in `parquet_kv_metadata`, and parts read through a glob;
- the published bytes are deterministic and match the provenance record;
- empty, garbage and truncated FCS files are refused and leave nothing.

The oracle is an independent FCS DATA reader below (`FcsData`), not cytiq.
Run it with the interpreter of `parity/lakehouse/requirements.lock`
(`just lakehouse-interop`).
"""

from __future__ import annotations

import argparse
import hashlib
import json
import mmap
from pathlib import Path
import struct
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
KEYS = ("acquisition_id", "layer_id", "source_event_id")
WIDTHS = {"F": 4, "D": 8}
SQL_TYPES = {"F": "FLOAT", "D": "DOUBLE", "I": "UBIGINT"}


def sha256(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


class FcsData:
    """The parameters and row-major little-endian DATA of an FCS file."""

    def __init__(self, path):
        self.path = Path(path)
        with self.path.open("rb") as handle:
            header = handle.read(58).decode("ascii")
            text_start, text_end = int(header[10:18]), int(header[18:26])
            handle.seek(text_start)
            raw = handle.read(text_end - text_start + 1).decode("utf-8")
        delimiter = raw[0]
        fields = raw[1:].split(delimiter)
        pairs = {key.upper(): value for key, value in zip(fields[::2], fields[1::2])}
        if pairs.get("$BYTEORD") != "1,2,3,4":
            raise ValueError("the oracle reads little-endian files only")
        self.events = int(pairs["$TOT"])
        self.begin = int(pairs["$BEGINDATA"])
        datatype = pairs["$DATATYPE"]
        self.channels = []
        offset = 0
        for n in range(1, int(pairs["$PAR"]) + 1):
            kind = pairs.get(f"$P{n}DATATYPE", datatype)
            width = int(pairs[f"$P{n}B"]) // 8
            if kind != "I" and width != WIDTHS[kind]:
                raise ValueError(f"$P{n}B does not match its type")
            self.channels.append({"name": pairs[f"$P{n}N"], "kind": kind, "width": width,
                                  "offset": offset})
            offset += width
        self.row_bytes = offset

    def column(self, index):
        """Little-endian bytes of channel `index`, integers widened to 64 bits."""
        channel = self.channels[index]
        width = channel["width"]
        out = bytearray((8 if channel["kind"] == "I" else width) * self.events)
        stride = 8 if channel["kind"] == "I" else width
        with self.path.open("rb") as handle, mmap.mmap(
                handle.fileno(), 0, access=mmap.ACCESS_READ) as data:
            start = self.begin + channel["offset"]
            end = self.begin + self.events * self.row_bytes
            for byte in range(width):
                out[byte::stride] = data[start + byte:end:self.row_bytes]
        return bytes(out)


def arrow_bytes(array, width):
    """The values buffer of a non-null fixed-width Arrow array."""
    array = array.combine_chunks() if hasattr(array, "combine_chunks") else array
    if array.null_count:
        raise AssertionError("a channel holds nulls")
    buffer = array.buffers()[1]
    return buffer.to_pybytes()[array.offset * width:(array.offset + len(array)) * width]


class Checks:
    def __init__(self):
        self.failures = []
        self.passed = 0

    def that(self, condition, message):
        if condition:
            self.passed += 1
        else:
            self.failures.append(message)
            print(f"FAIL {message}", flush=True)


def cytiq(binary, *args):
    return subprocess.run([str(binary), *map(str, args)], capture_output=True, text=True,
                          check=False)


def export(binary, source, output, *extra):
    result = cytiq(binary, "export", "events", "--input", source, "--output", output,
                   "--layer", "raw", *extra)
    if result.returncode:
        raise RuntimeError(f"export of {source} failed: {result.stderr}")


def parquet_files(output):
    output = Path(output)
    return sorted(output.glob("part-*.parquet")) if output.is_dir() else [output]


def check_table(checks, name, fcs, output):
    import duckdb
    import pyarrow.parquet as pq

    files = parquet_files(output)
    source = str(output / "part-*.parquet") if Path(output).is_dir() else str(output)
    dataset = pq.ParquetDataset([str(f) for f in files])
    schema = dataset.schema
    channels = [c["name"] for c in fcs.channels]
    checks.that(schema.names == [*KEYS, *channels], f"{name}: column order")
    acquisition = sha256(fcs.path)
    keys = dataset.read(columns=list(KEYS))
    ids = keys.column("source_event_id").combine_chunks()
    checks.that(arrow_bytes(ids, 8) == struct.pack(f"<{fcs.events}Q", *range(fcs.events)),
                f"{name}: source_event_id is 0..n in order")
    checks.that(set(keys.column("acquisition_id").unique().to_pylist()) == {acquisition},
                f"{name}: every row names the acquisition")
    checks.that(len(keys.column("layer_id").unique()) == 1, f"{name}: one layer")
    del keys, ids
    for index, channel in enumerate(fcs.channels):
        column = dataset.read(columns=[channel["name"]]).column(0)
        width = 8 if channel["kind"] == "I" else channel["width"]
        checks.that(arrow_bytes(column, width) == fcs.column(index),
                    f"{name}: `{channel['name']}` is bit-exact in PyArrow")

    connection = duckdb.connect()
    described = dict(connection.execute(
        f"SELECT column_name, column_type FROM (DESCRIBE SELECT * FROM read_parquet('{source}'))"
    ).fetchall())
    for channel in fcs.channels:
        checks.that(described.get(channel["name"]) == SQL_TYPES[channel["kind"]],
                    f"{name}: DuckDB types `{channel['name']}` as {SQL_TYPES[channel['kind']]}")
    count, low, high = connection.execute(
        f"SELECT count(*), min(source_event_id), max(source_event_id) "
        f"FROM read_parquet('{source}')").fetchone()
    checks.that((count, low, high) == (fcs.events, 0, fcs.events - 1),
                f"{name}: DuckDB counts every event once")
    first = channels[-1].replace('"', '""')
    rows = connection.execute(
        f'SELECT "{first}" FROM read_parquet(\'{source}\') ORDER BY source_event_id LIMIT 6'
    ).fetchall()
    checks.that(len(rows) == min(6, fcs.events), f"{name}: DuckDB reads a quoted column")
    metadata = dict(connection.execute(
        f"SELECT decode(key), decode(value) FROM parquet_kv_metadata('{files[0]}')").fetchall())
    dictionary = json.loads(metadata["cytiq.events.channels"])
    checks.that([c["name"] for c in dictionary] == channels,
                f"{name}: the channel dictionary is readable in SQL")
    return connection, source


def check_special_rows(checks, name, fcs, connection, source):
    """DuckDB keeps -0.0, NaN, infinities and integers above 2^63."""
    for channel in fcs.channels:
        quoted = channel["name"].replace('"', '""')
        values = [row[0] for row in connection.execute(
            f'SELECT "{quoted}" FROM read_parquet(\'{source}\') '
            f"ORDER BY source_event_id LIMIT 6").fetchall()]
        if channel["kind"] == "I":
            expected = [0, 1, (1 << 53) | 1, 1 << 63, (1 << 64) - 2, (1 << 64) - 1]
            checks.that(values == expected, f"{name}: DuckDB UBIGINT `{channel['name']}`")
        else:
            checks.that(struct.pack("<d", values[0]) == struct.pack("<d", -0.0),
                        f"{name}: DuckDB keeps -0.0 in `{channel['name']}`")
            checks.that(values[1] != values[1] and values[2] != values[2],
                        f"{name}: DuckDB keeps NaN in `{channel['name']}`")
            checks.that(values[3] in (float("inf"), float("-inf")),
                        f"{name}: DuckDB keeps infinities in `{channel['name']}`")


def check_provenance(checks, name, output):
    output = Path(output)
    record = (output / "provenance.json") if output.is_dir() else \
        Path(f"{output}.provenance.json")
    recorded = {Path(o["path"]).name: o["sha256"]
                for o in json.loads(record.read_text())["outputs"]}
    files = parquet_files(output) + ([output / "parts.json"] if output.is_dir() else [])
    checks.that(all(recorded.get(f.name) == sha256(f) for f in files) and
                len(recorded) == len(files), f"{name}: checksums match the provenance record")


def write_names_fcs(path, names, events):
    """A float32 FCS 3.1 file whose `$PnN` are unusual."""
    pairs = [("$BEGINANALYSIS", "0"), ("$ENDANALYSIS", "0"), ("$BEGINSTEXT", "0"),
             ("$ENDSTEXT", "0"), ("$BEGINDATA", "0" * 20), ("$ENDDATA", "0" * 20),
             ("$BYTEORD", "1,2,3,4"), ("$DATATYPE", "F"), ("$MODE", "L"),
             ("$NEXTDATA", "0"), ("$PAR", str(len(names))), ("$TOT", str(events))]
    for n, channel in enumerate(names, 1):
        pairs += [(f"$P{n}B", "32"), (f"$P{n}E", "0,0"), (f"$P{n}N", channel),
                  (f"$P{n}R", "262144")]

    def text():
        return ("|" + "".join(f"{k}|{v}|" for k, v in pairs)).encode("utf-8")

    data = struct.pack(f"<{events * len(names)}f",
                       *[float(i % 977) / 3 for i in range(events * len(names))])
    begin = 58 + len(text())
    pairs[4] = ("$BEGINDATA", f"{begin:020}")
    pairs[5] = ("$ENDDATA", f"{begin + len(data) - 1:020}")
    body = text()
    header = f"FCS3.1    {58:>8}{57 + len(body):>8}{begin:>8}{begin + len(data) - 1:>8}{0:>8}{0:>8}"
    Path(path).write_bytes(header.encode("ascii") + body + data)


def generate(example, path, events, channels, types, special):
    command = [str(example), "generate", "--output", str(path), "--events", str(events),
               "--channels", str(channels), "--types", types]
    if special:
        command.append("--special")
    subprocess.run(command, check=True, capture_output=True)


def refusals(checks, binary, work):
    """Inputs that must be refused, leaving no output behind."""
    work.mkdir(parents=True, exist_ok=True)
    garbage = work / "garbage.fcs"
    garbage.write_bytes(b"FCS3.1    not an FCS file")
    empty = work / "empty.fcs"
    pairs = "|$BEGINDATA|0|$ENDDATA|0|$BYTEORD|1,2,3,4|$DATATYPE|F|$MODE|L|$NEXTDATA|0|" \
            "$PAR|1|$TOT|0|$P1B|32|$P1E|0,0|$P1N|A|$P1R|1|"
    empty.write_bytes(f"FCS3.1    {58:>8}{57 + len(pairs):>8}{0:>8}{0:>8}{0:>8}{0:>8}"
                      .encode() + pairs.encode())
    truncated = work / "truncated.fcs"
    source = (ROOT / "data/fixtures/flowcyt/Case12_P.fcs").read_bytes()
    truncated.write_bytes(source[:int(source[34:42]) - 10])
    for input_path in (garbage, empty, truncated):
        for extra in ([], ["--max-file-bytes", "1KiB"]):
            output = work / f"{input_path.stem}-out"
            result = cytiq(binary, "export", "events", "--input", input_path, "--output",
                           output, "--layer", "raw", *extra)
            checks.that(result.returncode == 2,
                        f"{input_path.name} {extra}: refused with exit 2, got {result.returncode}")
    leftovers = sorted(p.name for p in work.iterdir() if not p.name.endswith(".fcs"))
    checks.that(leftovers == [], f"refused exports leave nothing behind: {leftovers}")


def run(args):
    import duckdb
    import pyarrow

    checks = Checks()
    work = args.work
    work.mkdir(parents=True, exist_ok=True)
    cases = [
        ("f32_special", 1_000, 6, "float32", True),
        ("f64_special", 1_000, 4, "float64", True),
        ("mixed_special", 1_000, 8, "mixed", True),
        ("one_event", 1, 3, "mixed", False),
        ("f32_1m", 1_000_000, 12, "float32", False),
        (f"mixed_{args.events}", args.events, 30, "mixed", False),
    ]
    for name, events, channels, types, special in cases:
        source = work / f"{name}.fcs"
        if not source.exists():
            generate(args.example, source, events, channels, types, special)
        fcs = FcsData(source)
        single = work / f"{name}.parquet"
        parts = work / f"{name}.parts"
        for path in (single, Path(f"{single}.provenance.json")):
            path.unlink(missing_ok=True)
        if parts.exists():
            for child in parts.iterdir():
                child.unlink()
            parts.rmdir()
        export(args.cytiq, source, single)
        first = sha256(single)
        export(args.cytiq, source, single, "--overwrite")
        checks.that(sha256(single) == first, f"{name}: the same export gives the same bytes")
        row_bytes = 8 + sum(8 if c["kind"] == "I" else c["width"] for c in fcs.channels)
        per_part = max(1, events // 3) * row_bytes
        export(args.cytiq, source, parts, "--max-file-bytes", str(per_part), "--compression",
               "snappy")
        for output in (single, parts):
            label = f"{name} ({'parts' if output == parts else 'table'})"
            connection, sql_source = check_table(checks, label, fcs, output)
            if special:
                check_special_rows(checks, label, fcs, connection, sql_source)
            check_provenance(checks, label, output)
        # Exact and independent of row order: summed hashes of every value.
        # Floating sums would differ in the last bits with the grouping.
        fingerprints = []
        for output in (single, parts):
            sql_source = str(output / "part-*.parquet") if output.is_dir() else str(output)
            quoted = [c["name"].replace('"', '""') for c in fcs.channels]
            columns = ", ".join(f'sum(hash("{c}"))::VARCHAR' for c in quoted)
            fingerprints.append(duckdb.sql(
                f"SELECT count(*), sum(hash(source_event_id))::VARCHAR, {columns} "
                f"FROM read_parquet('{sql_source}')").fetchall()[0])
        checks.that(fingerprints[0] == fingerprints[1],
                    f"{name}: parts hold exactly the single table's values in SQL")
        print(f"{name}: {checks.passed} checks passed so far", flush=True)

    names_source = work / "names.fcs"
    names = ["FSC-A", "CD3 FITC", "CD4·PE-Cy7 (α)", 'Quote"d \'name\'', "a,b;c", "L" * 120]
    write_names_fcs(names_source, names, 100)
    names_output = work / "names.parquet"
    names_output.unlink(missing_ok=True)
    Path(f"{names_output}.provenance.json").unlink(missing_ok=True)
    export(args.cytiq, names_source, names_output)
    check_table(checks, "unusual names", FcsData(names_source), names_output)
    refusals(checks, args.cytiq, work / "refused")

    report = {"passed": checks.passed, "failures": checks.failures,
              "duckdb": duckdb.__version__, "pyarrow": pyarrow.__version__,
              "events": args.events}
    (work / "interop.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))
    return 1 if checks.failures else 0


def parse_args(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cytiq", type=Path, default=ROOT / "target/release/cytiq")
    parser.add_argument("--example", type=Path,
                        default=ROOT / "target/release/examples/events_layouts")
    parser.add_argument("--work", type=Path, default=ROOT / "target/lakehouse/interop")
    parser.add_argument("--events", type=int, default=1_000_000,
                        help="events of the large mixed-type case (10000000 weekly)")
    return parser.parse_args(argv)


if __name__ == "__main__":
    sys.exit(run(parse_args()))
