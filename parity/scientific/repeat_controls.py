#!/usr/bin/env python3
"""Synthetic matched repeat single-stain controls: compensation uncertainty
within and between controls, and what it does to populations.

No pinned dataset has repeated single-stain controls, so this experiment is
synthetic and is labelled so in its report; it never stands in for biological
evidence. `parity/scientific/README.md#required-real-data` specifies the real
data that would. Production AutoSpill and compensation are unchanged: every
matrix comes from `cytiq autospill` and every compensated sample from
`cytiq compensate`.

Model (per scenario, predeclared). A *set* is one session: per-detector gains
g_j = exp(N(0, gain_sd)). Within a set, K independently stained tubes per dye
vary in brightness exp(N(0, brightness_sd)), tandem donor emission
(PE-Cy7→PE-A and APC-Cy7→APC-A coefficients + N(0, tandem_sd), floored at 0)
and autofluorescence level/shape. A tube's true row is S_r[i, j]·g_j / g_i.
The stained sample has its own tandem draw and g = 1.

Three estimands are kept apart:
  tube truth     the row a given tube carries;
  set truth      E[tube truth | set], exact via the floored-normal mean;
  sample truth   the matrix the stained sample was acquired with.
Event-bootstrap intervals describe one tube's sampling noise; t-intervals over
K tubes describe the set truth; prediction intervals a new tube. None covers the
sample truth when sessions differ: that under-coverage is the finding.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import sys
import time

import numpy as np
import pandas as pd
from scipy import stats

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "parity/gen"))
sys.path.insert(0, str(Path(__file__).resolve().parent))

from datasets import sha256  # noqa: E402
from fcsio import read_fcs, write_fcs  # noqa: E402
from generate import AUTOSPILL_DYES, AUTOSPILL_SPILLOVER, PNR, _detector_noise, _scatter_cloud  # noqa: E402
from metrics import matrix_perturbation, wilson_interval  # noqa: E402
from run import invoke  # noqa: E402

SCHEMA_VERSION = 1
LABEL = "synthetic; not biological evidence"
AUTOFLUORESCENCE = np.array([300.0, 250.0, 120.0, 60.0, 40.0, 30.0])
TANDEMS = ((AUTOSPILL_DYES.index("PE-Cy7-A"), AUTOSPILL_DYES.index("PE-A")),
           (AUTOSPILL_DYES.index("APC-Cy7-A"), AUTOSPILL_DYES.index("APC-A")))
"""(dye, donor detector) cells whose coefficient drifts as a tandem degrades."""
SCENARIOS = {
    "null": {},
    "brightness": {"brightness_sd": 0.3},
    "tandem": {"tandem_sd": 0.01},
    "af": {"af_level_sd": 0.3, "af_shape_sd": 0.1},
    "gain": {"gain_sd": 0.05},
    "combined": {"brightness_sd": 0.3, "tandem_sd": 0.01, "af_level_sd": 0.3, "af_shape_sd": 0.1, "gain_sd": 0.05},
}
PARAMETERS = ("brightness_sd", "tandem_sd", "af_level_sd", "af_shape_sd", "gain_sd")
GATE_QUANTILE = 0.995
SEED = 20261118


def parameters(scenario: str) -> dict:
    return {name: SCENARIOS[scenario].get(name, 0.0) for name in PARAMETERS}


def tandem_row(rng, params) -> np.ndarray:
    """The spillover matrix with this tube's (or sample's) tandem draws."""
    spill = AUTOSPILL_SPILLOVER.copy()
    for dye, donor in TANDEMS:
        spill[dye, donor] = max(spill[dye, donor] + rng.normal(0.0, params["tandem_sd"]), 0.0) if params["tandem_sd"] else spill[dye, donor]
    return spill


def gain_normalised(spill: np.ndarray, gains: np.ndarray) -> np.ndarray:
    """S[i, j]·g_j / g_i: each row still has 1 on its own detector."""
    return spill * gains[None, :] / gains[:, None]


def set_truth(gains: np.ndarray, params: dict) -> np.ndarray:
    """E[tube truth | set]: tandem cells take the floored-normal mean."""
    spill = AUTOSPILL_SPILLOVER.copy()
    sigma = params["tandem_sd"]
    if sigma:
        for dye, donor in TANDEMS:
            mu = spill[dye, donor]
            spill[dye, donor] = mu * stats.norm.cdf(mu / sigma) + sigma * stats.norm.pdf(mu / sigma)
    return gain_normalised(spill, gains)


def write_tube(path: Path, rng, dye: int, spill: np.ndarray, gains: np.ndarray, params: dict, events: int):
    """One single-stain control: half positive, AF, debris, detector noise."""
    scatter, is_debris = _scatter_cloud(rng, events, 0.15)
    latent = np.zeros((events, len(AUTOSPILL_DYES)))
    positive = rng.random(events) < 0.5
    brightness = np.exp(rng.normal(0.0, params["brightness_sd"])) if params["brightness_sd"] else 1.0
    latent[positive, dye] = rng.lognormal(np.log(25_000 * brightness), 0.6, positive.sum())
    af_level = np.exp(rng.normal(0.0, params["af_level_sd"])) if params["af_level_sd"] else 1.0
    af_shape = np.exp(rng.normal(0.0, params["af_shape_sd"], len(AUTOSPILL_DYES))) if params["af_shape_sd"] else 1.0
    autofluorescence = AUTOFLUORESCENCE * af_level * af_shape
    signal = (latent @ spill + autofluorescence * rng.lognormal(0, 0.3, (events, 1))) * gains[None, :]
    signal[is_debris] = rng.lognormal(np.log(800), 1.2, (is_debris.sum(), len(AUTOSPILL_DYES)))
    write_fcs(path, np.column_stack([scatter, np.clip(_detector_noise(rng, signal), -PNR, PNR - 1)]),
              ["FSC-A", "SSC-A", *AUTOSPILL_DYES])


def estimate(binary: Path, directory: Path, files: list[str]) -> np.ndarray:
    """`cytiq autospill --matrix-kind spillover` on one tube per dye."""
    pd.DataFrame({"filename": files, "dye": AUTOSPILL_DYES}).to_csv(directory / "controls.csv", index=False)
    output = directory / "spillover.csv"
    invoke(binary, ["autospill", "--controls", directory / "controls.csv", "--input-dir", directory,
                    "--output", output, "--matrix-kind", "spillover"], directory / "autospill.log")
    matrix = pd.read_csv(output, index_col=0)
    if list(matrix.index) != AUTOSPILL_DYES or list(matrix.columns) != AUTOSPILL_DYES:
        raise ValueError(f"{output}: unexpected orientation")
    return matrix.to_numpy()


def bootstrap(binary: Path, directory: Path, files: list[str], replicates: int, seed: int) -> np.ndarray:
    """Production event bootstrap of one control set: resample each tube's events."""
    estimates = []
    originals = {name: read_fcs(directory / name) for name in files}
    for replicate in range(replicates):
        root = directory / f"bootstrap{replicate}"
        root.mkdir()
        rng = np.random.default_rng([seed, replicate])
        for name, (values, channels) in originals.items():
            write_fcs(root / name, values[np.sort(rng.integers(0, len(values), len(values)))], channels)
        estimates.append(estimate(binary, root, files))
        for name in files:
            (root / name).unlink()
    return np.stack(estimates)


# --------------------------------------------------------------------------- sample and populations
POPULATIONS = ("double_negative", "single_positive", "weak_pe", "fitc_bright_pe_dim")


def write_sample(path: Path, rng, spill: np.ndarray, events: int) -> tuple[np.ndarray, np.ndarray]:
    """Fully stained sample (g = 1): latent abundances and population labels."""
    pe, fitc = AUTOSPILL_DYES.index("PE-A"), AUTOSPILL_DYES.index("FITC-A")
    labels = rng.choice(len(POPULATIONS), size=events, p=[0.5, 0.3, 0.1, 0.1])
    latent = np.zeros((events, len(AUTOSPILL_DYES)))
    single = np.flatnonzero(labels == 1)
    latent[single, rng.integers(0, len(AUTOSPILL_DYES), single.size)] = rng.lognormal(np.log(25_000), 0.6, single.size)
    # Weak PE: 3× the robust SD of truth-compensated DN events in PE-A.
    probe = _noisy(np.random.default_rng(rng.integers(2**32)), np.zeros((20_000, len(AUTOSPILL_DYES))) @ spill
                   + AUTOFLUORESCENCE * np.random.default_rng(1).lognormal(0, 0.3, (20_000, 1))) @ np.linalg.inv(spill)
    p16, p84 = np.quantile(probe[:, pe], [0.16, 0.84])
    latent[labels == 2, pe] = 3.0 * (p84 - p16) / 2.0
    coexpressing = labels == 3
    latent[coexpressing, fitc] = rng.lognormal(np.log(40_000), 0.3, coexpressing.sum())
    latent[coexpressing, pe] = 3.0 * (p84 - p16) / 2.0
    scatter, _ = _scatter_cloud(rng, events, 0.0)
    signal = latent @ spill + AUTOFLUORESCENCE * rng.lognormal(0, 0.3, (events, 1))
    write_fcs(path, np.column_stack([scatter, np.clip(_noisy(rng, signal), -PNR, PNR - 1)]), ["FSC-A", "SSC-A", *AUTOSPILL_DYES])
    return latent, labels


def _noisy(rng, signal):
    return _detector_noise(rng, signal)


def compensate(binary: Path, sample: Path, matrix: np.ndarray, directory: Path) -> np.ndarray:
    """`cytiq compensate --matrix-kind spillover`; returns the detector columns."""
    directory.mkdir(parents=True)
    pd.DataFrame(matrix, index=AUTOSPILL_DYES, columns=AUTOSPILL_DYES).rename_axis("channel").to_csv(directory / "spillover.csv")
    output = directory / "compensated.fcs"
    invoke(binary, ["compensate", "--input", sample, "--output", output, "--matrix", directory / "spillover.csv",
                    "--matrix-kind", "spillover"], directory / "compensate.log")
    data, channels = read_fcs(output)
    return data[:, [channels.index(dye) for dye in AUTOSPILL_DYES]]


def population_metrics(values: np.ndarray, labels: np.ndarray, gates: np.ndarray, latent: np.ndarray) -> dict:
    pe, fitc = AUTOSPILL_DYES.index("PE-A"), AUTOSPILL_DYES.index("FITC-A")
    positive = values > gates[None, :]
    dn = labels == 0
    fitc_single = (labels == 1) & (latent[:, fitc] > 0)
    return {
        "dn_false_positive_rate": positive[dn].mean(axis=0).tolist(),
        "weak_pe_positive_fraction": float(positive[labels == 2, pe].mean()),
        "coexpressing_pe_positive_fraction": float(positive[labels == 3, pe].mean()),
        "fitc_single_pe_false_positive_rate": float(positive[fitc_single, pe].mean()),
        "population_medians": {POPULATIONS[p]: np.median(values[labels == p], axis=0).tolist() for p in range(len(POPULATIONS))},
    }


# --------------------------------------------------------------------------- intervals
def t_interval(values: np.ndarray, level: float = 0.95):
    """Mean ± t·sd/√K over axis 0."""
    k = values.shape[0]
    mean, sd = values.mean(axis=0), values.std(axis=0, ddof=1)
    half = stats.t.ppf(0.5 + level / 2, k - 1) * sd / np.sqrt(k)
    return mean - half, mean + half


def prediction_interval(values: np.ndarray, level: float = 0.95):
    """Mean ± t·sd·√(1 + 1/K): where a new tube from the same set should fall."""
    k = values.shape[0]
    mean, sd = values.mean(axis=0), values.std(axis=0, ddof=1)
    half = stats.t.ppf(0.5 + level / 2, k - 1) * sd * np.sqrt(1 + 1 / k)
    return mean - half, mean + half


def coverage(lower, upper, truth, mask) -> dict:
    inside = ((truth >= lower) & (truth <= upper))[mask]
    hits, total = int(inside.sum()), int(inside.size)
    low, high = wilson_interval(hits, total)
    return {"fraction": hits / total, "hits": hits, "intervals": total, "wilson95": [low, high]}


def run_scenario(binary: Path, root: Path, scenario: str, sets: int, repeats: int, bootstrap_sets: int,
                 bootstrap_replicates: int, events: int, sample_events: int) -> dict:
    params = parameters(scenario)
    rng = np.random.default_rng([SEED, list(SCENARIOS).index(scenario)])
    off = ~np.eye(len(AUTOSPILL_DYES), dtype=bool)
    estimates = np.empty((sets, repeats, *AUTOSPILL_SPILLOVER.shape))
    tube_truth = np.empty_like(estimates)
    set_truths = np.empty((sets, *AUTOSPILL_SPILLOVER.shape))
    bootstraps = []
    for s in range(sets):
        gains = np.exp(rng.normal(0.0, params["gain_sd"], len(AUTOSPILL_DYES))) if params["gain_sd"] else np.ones(len(AUTOSPILL_DYES))
        set_truths[s] = set_truth(gains, params)
        for k in range(repeats):
            directory = root / f"set{s:02d}" / f"repeat{k}"
            directory.mkdir(parents=True)
            files = []
            for dye in range(len(AUTOSPILL_DYES)):
                spill = tandem_row(rng, params)
                tube_truth[s, k, dye] = gain_normalised(spill, gains)[dye]
                name = f"ctrl_{dye}.fcs"
                write_tube(directory / name, rng, dye, spill, gains, params, events)
                files.append(name)
            estimates[s, k] = estimate(binary, directory, files)
            if s < bootstrap_sets and k == 0:
                bootstraps.append((s, bootstrap(binary, directory, files, bootstrap_replicates, SEED + s)))
            for name in files:
                (directory / name).unlink()

    sample_spill = tandem_row(rng, params)
    sample = root / "sample.fcs"
    latent, labels = write_sample(sample, rng, sample_spill, sample_events)
    truth_values = compensate(binary, sample, sample_spill, root / "compensate_truth")
    data, channels = read_fcs(sample)
    raw = data[:, [channels.index(dye) for dye in AUTOSPILL_DYES]]
    expected = raw @ np.linalg.inv(sample_spill)
    if np.max(np.abs(truth_values - expected) - (2.0 ** -24 * np.abs(expected) + 1e-9 * np.abs(raw) @ np.abs(np.linalg.inv(sample_spill)))) > 0:
        raise AssertionError("cytiq compensate does not reproduce raw · S⁻¹ within f32 rounding")
    gates = np.quantile(truth_values[labels == 0], GATE_QUANTILE, axis=0)
    reference = population_metrics(truth_values, labels, gates, latent)

    within_sd = np.sqrt(np.mean([b.std(axis=0, ddof=1) ** 2 for _, b in bootstraps], axis=0)) if bootstraps else None
    between_sd = np.sqrt(np.mean(estimates.var(axis=1, ddof=1), axis=0))
    sessions_sd = estimates.mean(axis=1).std(axis=0, ddof=1) if sets > 1 else None
    cells = {}
    for i in range(len(AUTOSPILL_DYES)):
        for j in range(len(AUTOSPILL_DYES)):
            if i == j:
                continue
            entry = {"mean": float(estimates[:, :, i, j].mean()), "between_tube_sd": float(between_sd[i, j]),
                     "between_session_sd": None if sessions_sd is None else float(sessions_sd[i, j]),
                     "sample_truth": float(sample_spill[i, j]), "reference": float(AUTOSPILL_SPILLOVER[i, j])}
            if within_sd is not None:
                entry["within_tube_sd"] = float(within_sd[i, j])
                entry["between_to_within_ratio"] = float(between_sd[i, j] / within_sd[i, j]) if within_sd[i, j] > 0 else None
                entry["excess_between_variance"] = float(between_sd[i, j] ** 2 - within_sd[i, j] ** 2)
            cells[f"{AUTOSPILL_DYES[i]}→{AUTOSPILL_DYES[j]}"] = entry

    mask = np.broadcast_to(off, (sets, *off.shape))
    t_low, t_high = zip(*[t_interval(estimates[s]) for s in range(sets)])
    p_low, p_high = zip(*[prediction_interval(estimates[s]) for s in range(sets)])
    t_low, t_high, p_low, p_high = map(np.stack, (t_low, t_high, p_low, p_high))
    sample_truth = np.broadcast_to(sample_spill, set_truths.shape)
    result_coverage = {
        "t_interval_vs_set_truth": coverage(t_low, t_high, set_truths, mask),
        "t_interval_vs_sample_truth": coverage(t_low, t_high, sample_truth, mask),
        "prediction_interval_vs_sample_truth": coverage(p_low, p_high, sample_truth, mask),
    }
    # A new tube of the same set: leave-one-out prediction intervals against
    # the held-out tube's own estimate.
    loo_low, loo_high, held = [], [], []
    for s in range(sets):
        for k in range(repeats):
            low, high = prediction_interval(np.delete(estimates[s], k, axis=0))
            loo_low.append(low)
            loo_high.append(high)
            held.append(estimates[s, k])
    result_coverage["loo_prediction_interval_vs_held_out_tube_estimate"] = coverage(
        np.stack(loo_low), np.stack(loo_high), np.stack(held), np.broadcast_to(off, (len(held), *off.shape)))
    if bootstraps:
        boot_mask = np.broadcast_to(off, (len(bootstraps), *off.shape))
        lows = np.stack([np.quantile(b, 0.025, axis=0) for _, b in bootstraps])
        highs = np.stack([np.quantile(b, 0.975, axis=0) for _, b in bootstraps])
        tubes = np.stack([tube_truth[s, 0] for s, _ in bootstraps])
        result_coverage["bootstrap_vs_tube_truth"] = coverage(lows, highs, tubes, boot_mask)
        result_coverage["bootstrap_vs_sample_truth"] = coverage(lows, highs, np.broadcast_to(sample_spill, tubes.shape), boot_mask)
        result_coverage["bootstrap_quality"] = "full" if bootstrap_replicates >= 1000 else f"exploratory; {bootstrap_replicates} < 1000 resamples"

    downstream, coefficient_error, fp_change, bounds = [], [], [], []
    fitc, pe = AUTOSPILL_DYES.index("FITC-A"), AUTOSPILL_DYES.index("PE-A")
    for s in range(sets):
        matrix = estimates[s, 0]
        values = compensate(binary, sample, matrix, root / f"compensate_set{s:02d}")
        metrics = population_metrics(values, labels, gates, latent)
        perturbation, _ = matrix_perturbation(sample_spill, latent, matrix - sample_spill)
        if perturbation["first_order_relative_error"] > perturbation["bound"] + 1e-12:
            raise AssertionError(f"first-order bound violated for set {s}")
        bounds.append({"first_order_relative_error": perturbation["first_order_relative_error"], "bound": perturbation["bound"],
                       "rmse": perturbation["rmse"]})
        coefficient_error.append(abs(matrix[fitc, pe] - sample_spill[fitc, pe]))
        fp_change.append(abs(metrics["fitc_single_pe_false_positive_rate"] - reference["fitc_single_pe_false_positive_rate"]))
        downstream.append({
            "set": s,
            "max_abs_coefficient_error": float(np.abs(matrix - sample_spill)[off].max()),
            "dn_false_positive_rate_max": float(max(metrics["dn_false_positive_rate"])),
            "weak_pe_positive_fraction_change": metrics["weak_pe_positive_fraction"] - reference["weak_pe_positive_fraction"],
            "coexpressing_pe_positive_fraction_change": metrics["coexpressing_pe_positive_fraction"] - reference["coexpressing_pe_positive_fraction"],
            "fitc_single_pe_false_positive_rate": metrics["fitc_single_pe_false_positive_rate"],
            "fitc_single_pe_false_positive_rate_change": metrics["fitc_single_pe_false_positive_rate"] - reference["fitc_single_pe_false_positive_rate"],
            "median_shift_max_abs": float(max(np.max(np.abs(np.array(metrics["population_medians"][p]) - np.array(reference["population_medians"][p])))
                                              for p in POPULATIONS)),
        })
    spearman = stats.spearmanr(coefficient_error, fp_change) if len(set(coefficient_error)) > 1 and len(set(fp_change)) > 1 else None
    summary = lambda key: {"min": float(min(d[key] for d in downstream)), "median": float(np.median([d[key] for d in downstream])),  # noqa: E731
                           "max": float(max(d[key] for d in downstream))}
    return {
        "parameters": params,
        "sets": sets, "repeats_per_set": repeats, "events_per_tube": events, "sample_events": sample_events,
        "bootstrap": {"sets": len(bootstraps), "replicates": bootstrap_replicates},
        "cells": cells,
        "max_between_tube_sd": float(between_sd[off].max()),
        "max_within_tube_sd": float(within_sd[off].max()) if within_sd is not None else None,
        "coverage": result_coverage,
        "downstream": {
            "gates": f"{GATE_QUANTILE} quantile of truth-compensated double negatives per detector",
            "reference_with_sample_truth": {k: v for k, v in reference.items() if k != "population_medians"},
            "per_set_first_repeat": downstream,
            "dn_false_positive_rate_max": summary("dn_false_positive_rate_max"),
            "weak_pe_positive_fraction_change": summary("weak_pe_positive_fraction_change"),
            "coexpressing_pe_positive_fraction_change": summary("coexpressing_pe_positive_fraction_change"),
            "fitc_single_pe_false_positive_rate": summary("fitc_single_pe_false_positive_rate"),
            "fitc_single_pe_false_positive_rate_change": summary("fitc_single_pe_false_positive_rate_change"),
            "median_shift_max_abs": summary("median_shift_max_abs"),
            "spearman_fitc_pe_coefficient_error_vs_false_positive_change": None if spearman is None else
                {"rho": float(spearman.statistic), "p_value": float(spearman.pvalue)},
            "first_order_bound": {"max_relative_error": max(b["first_order_relative_error"] for b in bounds),
                                  "max_bound": max(b["bound"] for b in bounds), "violations": 0},
        },
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("mode", choices=["run", "table"])
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--report", type=Path)
    parser.add_argument("--scenarios", default=",".join(SCENARIOS))
    parser.add_argument("--sets", type=int, default=20)
    parser.add_argument("--repeats", type=int, default=8)
    parser.add_argument("--bootstrap-sets", type=int, default=3)
    parser.add_argument("--bootstrap", type=int, default=100)
    parser.add_argument("--events", type=int, default=5000)
    parser.add_argument("--sample-events", type=int, default=50_000)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    if args.mode == "table":
        report = json.loads(args.report.read_text())
        print(table(report))
        (args.output / "table.md").write_text(table(report))
        return
    if args.binary is None:
        parser.error("--binary is required")
    report = {"schema_version": SCHEMA_VERSION, "experiment": "repeat_control_compensation", "label": LABEL,
              "status": "failed", "seed": SEED, "binary_sha256": sha256(args.binary),
              "scripts": {name: sha256(ROOT / name) for name in ("parity/scientific/repeat_controls.py", "parity/gen/generate.py", "parity/scientific/metrics.py")},
              "arguments": {k: str(v) for k, v in vars(args).items()},
              "real_repeat_controls": {"status": "not_assessed", "required_data": "parity/scientific/README.md#required-real-data"},
              "scenarios": {}}
    started = time.monotonic()
    try:
        for scenario in args.scenarios.split(","):
            report["scenarios"][scenario] = run_scenario(args.binary, args.output / scenario, scenario, args.sets, args.repeats,
                                                         args.bootstrap_sets, args.bootstrap, args.events, args.sample_events)
            (args.output / "progress.json").write_text(json.dumps(report, indent=2, allow_nan=False) + "\n")
        report["status"] = "completed"
    except Exception as error:
        report["error"] = f"{type(error).__name__}: {error}"
        raise
    finally:
        report["wall_seconds"] = time.monotonic() - started
        (args.output / "report.json").write_text(json.dumps(report, indent=2, allow_nan=False) + "\n")


def table(report: dict) -> str:
    def f(value, digits=3):
        return "–" if value is None else f"{value:.{digits}f}"
    lines = ["| scenario | max between-tube SD | max within-tube SD | t-int. → set truth | t-int. → sample truth | pred. int. → sample truth | bootstrap → tube truth | bootstrap → sample truth | DN FP max (median) | weak PE Δ (max abs) | FITC single → PE FP: truth / Δ max abs |",
             "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|"]
    for name, result in report["scenarios"].items():
        cov = result["coverage"]
        down = result["downstream"]
        weak = down["weak_pe_positive_fraction_change"]
        lines.append("| " + " | ".join([
            name, f(result["max_between_tube_sd"], 4), f(result["max_within_tube_sd"], 4),
            f(cov["t_interval_vs_set_truth"]["fraction"]), f(cov["t_interval_vs_sample_truth"]["fraction"]),
            f(cov["prediction_interval_vs_sample_truth"]["fraction"]),
            f(cov.get("bootstrap_vs_tube_truth", {}).get("fraction")), f(cov.get("bootstrap_vs_sample_truth", {}).get("fraction")),
            f(down["dn_false_positive_rate_max"]["median"], 4), f(max(abs(weak["min"]), abs(weak["max"])), 4),
            f"{f(down['reference_with_sample_truth']['fitc_single_pe_false_positive_rate'], 4)} / "
            f"{f(max(abs(down['fitc_single_pe_false_positive_rate_change']['min']), abs(down['fitc_single_pe_false_positive_rate_change']['max'])), 4)}"]) + " |")
    return "\n".join(lines) + "\n"


if __name__ == "__main__":
    main()
