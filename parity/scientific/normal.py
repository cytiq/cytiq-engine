#!/usr/bin/env python3
"""Subject-disjoint novelty validation on four published unstimulated donors.

Small denominators and fit failures are retained explicitly. This is a stress
test of insufficient references, not a clinically calibrated FPR estimate.
"""
import argparse
import json
from pathlib import Path
import time

import flowio
import pandas as pd

from datasets import sha256
from metrics import wilson_interval
from run import invoke

ROOT = Path(__file__).resolve().parents[2]


def run(binary, output):
    output.mkdir(parents=True, exist_ok=False)
    fixtures = ROOT / "data/fixtures/scientific/normal"
    files = [fixtures / f"sample_{n}.fcs" for n in (2, 4, 6, 8)]
    panel = pd.read_csv(fixtures / "panel.csv")
    channels = panel.loc[panel.marker_class == "type", "fcs_colname"].tolist()
    if len(channels) < 4:
        raise ValueError("insufficient declared lineage markers")
    report = {"schema_version": 1, "status": "failed", "binary_sha256": sha256(binary),
              "source_sha256": sha256(fixtures/"source.json"),
              "inputs": {p.name: sha256(p) for p in files}, "subjects": 4,
              "seed": 42, "normal_definition": "published unstimulated Ref samples from four healthy PBMC donors",
              "calibration_status": "insufficient_reference", "results": []}
    start = time.monotonic()
    try:
        for dimension in sorted({4, len(channels)}):
            for method in ("summary", "quantiles", "emd", "binning", "fingerprint"):
                hits = 0
                assessed = 0
                failures = []
                for i, test in enumerate(files):
                    case = output / f"{dimension}-{method}-{i}"
                    args = ["cytoscan", "--output", case, "--channels", ",".join(channels[:dimension]),
                            "--method", method, "--flag-method", "novelty", "--input", test]
                    for ref in files:
                        if ref != test:
                            args += ["--reference", ref]
                    try:
                        invoke(binary, args, output/f"{case.name}.log")
                    except Exception as error:
                        failures.append({"subject": i+1, "error": str(error)})
                        continue
                    summary = pd.read_csv(case/"cytoscan_summary.csv")
                    if len(summary) != 1:
                        raise ValueError("held-out subject count changed")
                    hits += int(str(summary.iloc[0]["is_novelty"]).lower() in ("true", "1"))
                    assessed += 1
                report["results"].append({"channels": dimension, "method": method,
                    "reference_subjects": 3, "normal_subjects": 4, "assessed": assessed,
                    "fit_failures": failures, "false_positives": hits,
                    "fpr": hits/assessed if assessed else None,
                    "wilson_ci95": wilson_interval(hits, assessed) if assessed else None,
                    "status": "assessed" if assessed == 4 else "incomplete"})
        report["status"] = "completed" if all(r["assessed"] == 4 for r in report["results"]) else "fit_failures"
    finally:
        report["wall_seconds"] = time.monotonic()-start
        (output/"report.json").write_text(json.dumps(report, indent=2, allow_nan=False)+"\n")
    if report["status"] != "completed":
        raise SystemExit("required normal acquisitions could not all be scored; inspect fit failures")


if __name__ == "__main__":
    p = argparse.ArgumentParser()
    p.add_argument("--binary", type=Path, required=True)
    p.add_argument("--output", type=Path, required=True)
    a = p.parse_args()
    run(a.binary, a.output)
