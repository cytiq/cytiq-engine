#!/usr/bin/env python3
"""Expose persisted Rust coefficients to the independent R parity oracle."""
import json
import sys
from collections import defaultdict
from pathlib import Path

import pyarrow.parquet as pq

curves = defaultdict(list)
for row in pq.read_table(Path(sys.argv[1]) / "splines.parquet").to_pylist():
    if row["identity_reason"] is None:
        curves[(row["batch"], row["metacluster"], row["marker"])].append(row)
result = []
for (batch, cluster, marker), rows in sorted(curves.items()):
    rows.sort(key=lambda row: row["knot"])
    result.append({"batch": batch, "cluster": cluster, "marker": marker,
                   **{key: [row[key] for row in rows] for key in ("x", "y", "slope")}})
Path(sys.argv[2]).write_text(json.dumps(result) + "\n")
