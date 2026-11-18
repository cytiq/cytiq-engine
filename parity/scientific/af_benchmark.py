#!/usr/bin/env python3
"""Autofluorescence (AF) reference benchmark: production hard argmax against
alternative and multi-component estimators.

Every mode first runs `cytiq autospectral --debug-dir` and checks that the
NumPy port in `af_estimators.py` reproduces the production event selection and
spectra (`oracle`); alternative arms are then fitted on the same gated events.
Nothing here changes production. An arm is a production candidate only if it
meets the predeclared `DECISION_RULE`, which requires independent real data
this repository does not have; reports therefore record every arm as not
eligible, with the failed criteria.

Modes:
  oracle     production vs the port on one dataset
  synthetic  scenarios with known AF truth (af_scenarios.py)
  real       a real spectral control set (e.g. aurora_pbmc_spectral)
  table      markdown comparison table from reports
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import sys
import time

import numpy as np
import pandas as pd
import scipy

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "parity/gen"))
sys.path.insert(0, str(Path(__file__).resolve().parent))

import af_estimators as est  # noqa: E402
from af_sensitivity import subset  # noqa: E402
from datasets import sha256  # noqa: E402
from fcsio import read_fcs  # noqa: E402
from run import invoke  # noqa: E402

SCHEMA_VERSION = 1
ORACLE_TOLERANCE = 1e-9
"""Absolute tolerance of the port against production. Never widened: a failure
means the port diverged from the Rust code and must be fixed."""

DECISION_RULE = {
    "version": 1,
    "baseline": est.BASELINE_ARM,
    "oracle_must_pass": True,
    "multi_af_must_be_identifiable": True,
    "held_out_median_robust_sd_ratio_max": {
        "single_af": 1.01, "two_af_near_tie": 0.95, "minority_af_scatter_linked": 0.95,
        "minority_af_scatter_independent": 0.95, "weak_positive": 1.01, "real": 0.95},
    "held_out_p99_ratio_max_any_component": 1.05,
    "split_half_min_cosine_floor": 0.999,
    "condition_ratio_max": 1.25,
    "af_fluorophore_cosine_increase_max": 0.01,
    "synthetic_criteria_hold_on_every_seed": True,
    "independent_real_datasets_min": 2,
    "outcome": "an eligible arm becomes a separate reviewed proposal stating its R parity impact; nothing is adopted automatically",
}
DECISION_RULE_SHA256 = hashlib.sha256(json.dumps(DECISION_RULE, sort_keys=True).encode()).hexdigest()

SCRIPTS = ["parity/scientific/af_benchmark.py", "parity/scientific/af_estimators.py",
           "parity/scientific/af_scenarios.py", "parity/scientific/af_sensitivity.py",
           "parity/gen/generate.py"]


# --------------------------------------------------------------------------- inputs
class Source:
    """A control set in either layout: AutoSpectral's (`control_file.csv`, `controls/`,
    `sample/`) or cytiq's (`controls.csv`, `samples.csv` beside the files)."""

    def __init__(self, path: Path):
        self.path = path
        if (path / "control_file.csv").exists():
            self.controls = path / "control_file.csv"
            self.input_dir = path / "controls"
            self.samples = sorted((path / "sample").glob("*.fcs"))
        else:
            self.controls = path / "controls.csv"
            self.input_dir = path
            self.samples = [path / name for name in pd.read_csv(path / "samples.csv", dtype=str).filename]
        if len(self.samples) != 1:
            raise ValueError(f"{path}: the benchmark needs exactly one sample, found {len(self.samples)}")
        self.manifest = pd.read_csv(self.controls, dtype=str, keep_default_na=False)

    def control(self, filename: str) -> Path:
        return self.input_dir / filename

    def hashes(self) -> dict:
        files = [self.controls, *self.samples, *[self.control(n) for n in self.manifest.filename]]
        return {os.path.relpath(f, self.path): sha256(f) for f in files}


def run_production(binary: Path, source: Source, root: Path, options: dict,
                   af_override: Path | None = None, sample: Path | None = None) -> dict:
    """`cytiq autospectral --debug-dir` with the AF control optionally replaced."""
    root.mkdir(parents=True)
    manifest = source.manifest.copy()
    af_names = {name for name, dye in zip(manifest.filename, manifest.get("dye", manifest.get("fluorophore")))
                if dye.strip().lower() in ("af", "autofluorescence")}

    def resolve(name: str) -> str:
        path = af_override if af_override is not None and name in af_names else source.control(name)
        return os.path.relpath(path, root)

    manifest["filename"] = [resolve(name) for name in manifest.filename]
    if "universal.negative" in manifest:
        manifest["universal.negative"] = [resolve(name) if name else "" for name in manifest["universal.negative"]]
    manifest.to_csv(root / "controls.csv", index=False)
    pd.DataFrame({"filename": [os.path.relpath(sample or source.samples[0], root)]}).to_csv(root / "samples.csv", index=False)
    args = ["autospectral", "--controls", root / "controls.csv", "--samples", root / "samples.csv",
            "--input-dir", root, "--output", root / "out", "--debug-dir", root / "debug",
            "--fsc", options["fsc"], "--ssc", options["ssc"]]
    if options["quantile_gate"]:
        args.append("--quantile-gate")
    invoke(binary, args, root / "run.log")
    references = json.loads((root / "out/references/autospectral-references.json").read_text())
    return {
        "root": root,
        "run": json.loads((root / "out/diagnostics/run.json").read_text()),
        "events": json.loads((root / "debug/reference_events.json").read_text()),
        "names": [c["name"] for c in references["components"]],
        "reference": np.array([c["spectrum"] for c in references["components"]], dtype=np.float64).T,
        "unmixed": next((root / "out/unmixed").glob("*.fcs")),
    }


class Acquisition:
    """An FCS file's events with detector and scatter columns located by exact $PnN."""

    def __init__(self, path: Path, detectors: list[str], fsc: str, ssc: str):
        data, names = read_fcs(path)
        index = {name: i for i, name in enumerate(names)}
        missing = [d for d in [*detectors, fsc, ssc] if d not in index]
        if missing:
            raise ValueError(f"{path}: channels missing: {missing}")
        self.detectors = data[:, [index[d] for d in detectors]]
        self.scatter = data[:, [index[fsc], index[ssc]]]


# --------------------------------------------------------------------------- oracle
def _max_abs(left, right) -> float:
    return float(np.max(np.abs(np.asarray(left, dtype=np.float64) - np.asarray(right, dtype=np.float64))))


def _bitwise(left, right) -> bool:
    return np.array_equal(np.asarray(left, dtype=np.float64).view(np.uint64),
                          np.asarray(right, dtype=np.float64).view(np.uint64))


def oracle_check(production: dict, options: dict) -> dict:
    """Recompute production's AF means, peak, runner-up, sensitivity, positive and
    negative selection and every spectrum from the exported rows."""
    events = production["events"]
    run = production["run"]
    detectors = events["detectors"]
    names = production["names"]
    reference = production["reference"]
    af_events = events["autofluorescence"]
    af = Acquisition(control_path(production, af_events["filename"]), detectors, options["fsc"], options["ssc"])
    af_rows = np.asarray(af_events["retained"], dtype=np.int64)
    Y_af = af.detectors[af_rows]
    means = est.mean_spectrum(Y_af)
    peak = est.first_argmax(means)
    runner_up = est.first_argmax(means, peak)
    af_spectrum = est.spectrum(est.columns_of(Y_af), peak)
    alternative = est.spectrum(est.columns_of(Y_af), runner_up)
    sensitivity = run["reference_diagnostics"].get("af_peak_sensitivity", {})
    deviations = {
        "af_detector_means": _max_abs(means, run["reference_diagnostics"]["af_detector_means"]),
        "af_spectrum": _max_abs(af_spectrum, reference[:, names.index("Autofluorescence")]),
        "relative_margin": abs(float((means[peak] - means[runner_up]) / abs(means[peak])) - sensitivity["relative_margin"]),
        "runner_up_spectrum_cosine": abs(float(est.cosine_sequential(af_spectrum, alternative)) - sensitivity["runner_up_spectrum_cosine"]),
    }
    bitwise = {
        "af_detector_means": _bitwise(means, run["reference_diagnostics"]["af_detector_means"]),
        "af_spectrum": _bitwise(af_spectrum, reference[:, names.index("Autofluorescence")]),
    }
    exact = {
        "peak_detector": peak == af_events["peak_detector"] and detectors[peak] == run["reference_diagnostics"]["af_peak_detector"],
        "runner_up_detector": runner_up == af_events["runner_up_detector"],
    }
    negative_scatter = af.scatter[af_rows]
    for stain in events["single_stains"]:
        control = Acquisition(control_path(production, stain["filename"]), detectors, options["fsc"], options["ssc"])
        primary = stain["primary_detector"]
        retained = np.asarray(stain["retained"], dtype=np.int64)
        positive = est.positive_rows(control.detectors[:, primary], retained, Y_af[:, primary])
        exact[f"positive[{stain['dye']}]"] = np.array_equal(positive, stain["positive"])
        neighbours = est.scatter_neighbours(control.scatter[positive], negative_scatter)
        matched = est.evenly_spaced(af_rows[neighbours], est.NEGATIVE_EVENT_LIMIT)
        exact[f"matched_negative[{stain['dye']}]"] = np.array_equal(matched, stain["matched_negative"])
        pooled = np.vstack([control.detectors[positive], af.detectors[matched]])
        fitted = est.spectrum(est.columns_of(pooled), primary)
        expected = reference[:, names.index(stain["dye"])]
        deviations[f"spectrum[{stain['dye']}]"] = _max_abs(fitted, expected)
        bitwise[f"spectrum[{stain['dye']}]"] = _bitwise(fitted, expected)
    ordered = np.sort(means)[::-1]
    passed = all(exact.values()) and all(value <= ORACLE_TOLERANCE for value in deviations.values())
    return {
        "status": "passed" if passed else "failed",
        "tolerance": ORACLE_TOLERANCE,
        "max_abs_deviation": deviations,
        "bitwise_equal": bitwise,
        "exact": {key: bool(value) for key, value in exact.items()},
        "top_mean_tie": bool(ordered.size > 1 and ordered[0] == ordered[1]),
        "control_gate": events["control_gate"],
    }


def control_path(production: dict, exported: str) -> Path:
    """A control's file: the export records the manifest filename, relative to the run."""
    return production["root"] / exported


def require_oracle(result: dict):
    if result["status"] != "passed":
        raise AssertionError(f"port diverged from production: {json.dumps(result, indent=2)}")


# --------------------------------------------------------------------------- evaluation
SOLVERS = ("joint", "min_residual", "upstream_score")
"""Multi-AF unmixing: every AF as a column, or one AF per event (smallest residual,
or AutoSpectral's `assign.af.residuals` score)."""


def variants(arms: dict) -> dict:
    """Arm × solver combinations: single-AF arms have one AF column, so only `joint`."""
    out = {}
    for name in arms:
        for solver in (("joint",) if name in est.SINGLE_AF_ARMS else SOLVERS):
            out[name if name in est.SINGLE_AF_ARMS else f"{name}+{solver}"] = (name, solver)
    return out


def unmix_variant(Y, fluorophores, afs, solver):
    """Abundances (fluorophores first) and residual norms of `Y` with `afs`."""
    if solver == "joint":
        abundances, residuals, _ = est.unmix_joint(Y, fluorophores, afs)
    elif solver == "min_residual":
        abundances, residuals, _ = est.unmix_min_residual(Y, fluorophores, afs)
    else:
        assignment = est.assign_upstream_score(Y, fluorophores, afs)
        abundances, residuals = est.unmix_assigned(Y, fluorophores, afs, assignment)
    return abundances, residuals


def fit_arm(arm, Y, S, seed=1):
    """(spectra, info) or (None, error) when the arm cannot estimate a spectrum."""
    try:
        spectra, info = arm(Y, S, seed=seed)
    except ValueError as error:
        return None, {"error": str(error)}
    if not np.all(np.isfinite(spectra)):
        return None, {"error": "non-finite spectrum", **{k: v for k, v in info.items() if k != "labels"}}
    return spectra, info


def summary(values) -> dict | None:
    values = np.asarray([v for v in values if v is not None and np.isfinite(v)], dtype=np.float64)
    if values.size == 0:
        return None
    return {"min": float(values.min()), "p05": float(np.quantile(values, 0.05)),
            "median": float(np.median(values)), "max": float(values.max()), "n": int(values.size)}


def stability(arms, Y, S, full, seeds, sizes) -> dict:
    """Arms refitted on subsamples and split halves of the fixed production gate: cosine
    to the arm's own full-data estimate (components matched), and peak switches."""
    n = Y.shape[0]
    result = {name: {"subsample": {}, "split_half_min_component_cosine": None, "failures": 0} for name in arms}
    for size in [size for size in (1_000, 10_000) if size < n]:
        cosines = {name: [] for name in arms}
        switches = {name: [] for name in arms}
        for seed in seeds:
            rows = np.sort(np.random.default_rng(seed).choice(n, size, replace=False))
            for name, arm in arms.items():
                spectra, info = fit_arm(arm, Y[rows], S[rows])
                if spectra is None or full[name][0] is None:
                    result[name]["failures"] += 1
                    continue
                cosines[name].append(float(est.matched_cosines(spectra, full[name][0]).min()))
                if "peak_detector" in info:
                    switches[name].append(info["peak_detector"] != full[name][1]["peak_detector"])
        for name in arms:
            result[name]["subsample"][str(size)] = {
                "cosine_to_full": summary(cosines[name]),
                "peak_switch_rate": float(np.mean(switches[name])) if switches[name] else None}
    for name, arm in arms.items():
        halves = []
        for seed in seeds:
            order = np.random.default_rng(10_000 + seed).permutation(n)
            first, second = np.sort(order[: n // 2]), np.sort(order[n // 2:])
            a, _ = fit_arm(arm, Y[first], S[first])
            b, _ = fit_arm(arm, Y[second], S[second])
            if a is None or b is None:
                result[name]["failures"] += 1
                continue
            halves.append(float(est.matched_cosines(a, b).min()))
        result[name]["split_half"] = summary(halves)
        result[name]["split_half_min_component_cosine"] = summary(halves)["min"] if halves else None
    return result


def peak_margin_bootstrap(Y, peak, runner_up, replicates=1_000, seed=42) -> dict:
    """Event bootstrap of the gated means: the peak's relative margin and how often
    another detector would take the argmax."""
    rng = np.random.default_rng(seed)
    margins, switches = [], 0
    for _ in range(replicates):
        means = Y[rng.integers(0, Y.shape[0], Y.shape[0])].mean(axis=0)
        margins.append((means[peak] - means[runner_up]) / abs(means[peak]))
        switches += int(np.argmax(means) != peak)
    low, high = np.quantile(margins, [0.025, 0.975])
    return {"relative_margin_ci95": [float(low), float(high)], "switch_probability": switches / replicates,
            "replicates": replicates}


def unmixing_metrics(X, residuals, Y, fluor_count) -> dict:
    fluor = X[:, :fluor_count]
    return {
        "median": [float(v) for v in np.median(fluor, axis=0)],
        "robust_sd": [est.robust_sd(fluor[:, j]) for j in range(fluor_count)],
        "p99_abs": [float(v) for v in np.quantile(np.abs(fluor), 0.99, axis=0)],
        "relative_residual_rms": float(np.sqrt(np.sum(residuals ** 2) / np.sum(Y ** 2))),
    }


def held_out(binary, source, root, options, af, af_file, gate_full, arms, seeds, spikes, sample, fluor_names) -> dict:
    """Fit on one half of the AF control, evaluate on the other half's gated events.
    Fluorophore spectra are rebuilt by production on the fitting half, so the evaluated
    events never served as their negatives. Unstained events should unmix to zero
    fluorophore signal (AutoSpectral's `benchmark_af_spectra` criterion)."""
    total = af.detectors.shape[0]
    in_gate = np.zeros(total, dtype=bool)
    in_gate[gate_full] = True
    names = variants(arms)
    runs = {variant: [] for variant in names}
    spike = {variant: [] for variant in names}
    checks = []
    for seed in seeds:
        order = np.random.default_rng(seed).permutation(total)
        halves = (np.sort(order[: total // 2]), np.sort(order[total // 2:]))
        for fit_index in (0, 1):
            fit_rows, eval_rows = halves[fit_index], halves[1 - fit_index]
            run_root = root / f"seed{seed}-fold{fit_index}"
            run_root.mkdir(parents=True)
            subset(af_file, run_root / "af.fcs", fit_rows)
            production = run_production(binary, source, run_root / "production", options,
                                        af_override=run_root / "af.fcs", sample=sample)
            fluorophores = production["reference"][:, :-1]
            gate_fit = fit_rows[np.asarray(production["events"]["autofluorescence"]["retained"], dtype=np.int64)]
            Y_fit, S_fit = af.detectors[gate_fit], af.scatter[gate_fit]
            evaluated = eval_rows[in_gate[eval_rows]]
            Y_eval = af.detectors[evaluated]
            fitted = {name: fit_arm(arm, Y_fit, S_fit)[0] for name, arm in arms.items()}
            checks.append(float(np.max(np.abs(fitted[est.BASELINE_ARM][0] - production["reference"][:, -1]))))
            baseline_X, _ = unmix_variant(Y_eval, fluorophores, fitted[est.BASELINE_ARM], "joint")
            amounts = {(j, m): m * est.robust_sd(baseline_X[:, j]) for j in range(len(fluor_names)) for m in spikes}
            for variant, (arm, solver) in names.items():
                if fitted[arm] is None:
                    continue
                X, residuals = unmix_variant(Y_eval, fluorophores, fitted[arm], solver)
                runs[variant].append(unmixing_metrics(X, residuals, Y_eval, len(fluor_names)))
                for (j, m), amount in amounts.items():
                    spiked, _ = unmix_variant(Y_eval + amount * fluorophores[:, j], fluorophores, fitted[arm], solver)
                    spike[variant].append({"fluorophore": j, "multiple": m,
                                           "auc": est.auc(spiked[:, j], X[:, j]),
                                           "relative_median_bias": float((np.median(spiked[:, j]) - np.median(X[:, j]) - amount) / amount)})
    if max(checks) > ORACLE_TOLERANCE:
        raise AssertionError(f"held-out production AF not reproduced: {max(checks)}")
    result = {}
    for variant in names:
        if not runs[variant]:
            result[variant] = {"status": "failed", "runs": 0}
            continue
        stack = lambda key: np.array([run[key] for run in runs[variant]])  # noqa: E731
        by_multiple = {}
        for m in spikes:
            entries = [e for e in spike[variant] if e["multiple"] == m]
            by_multiple[str(m)] = {
                "auc_median": float(np.median([e["auc"] for e in entries])),
                "auc_min": float(np.min([e["auc"] for e in entries])),
                "relative_median_bias_max_abs": float(np.max(np.abs([e["relative_median_bias"] for e in entries])))}
        result[variant] = {
            "status": "ok", "runs": len(runs[variant]),
            "robust_sd": stack("robust_sd").mean(axis=0).tolist(),
            "abs_median": np.abs(stack("median")).mean(axis=0).tolist(),
            "p99_abs": stack("p99_abs").mean(axis=0).tolist(),
            "relative_residual_rms": float(stack("relative_residual_rms").mean()),
            "weak_positive_spike_in": {"label": "semi_synthetic: a noise-free fluorophore signal added to held-out unstained events; optimistic (no added photon noise)",
                                       "by_multiple_of_baseline_robust_sd": by_multiple},
        }
    baseline = result[est.BASELINE_ARM]
    for variant, metrics in result.items():
        if metrics["status"] != "ok":
            continue
        sd_ratio = np.array(metrics["robust_sd"]) / np.array(baseline["robust_sd"])
        p99_ratio = np.array(metrics["p99_abs"]) / np.array(baseline["p99_abs"])
        metrics["robust_sd_ratio"] = sd_ratio.tolist()
        metrics["median_robust_sd_ratio"] = float(np.median(sd_ratio))
        metrics["max_p99_ratio"] = float(p99_ratio.max())
    return {"folds": 2, "seeds": list(seeds), "production_af_reproduced_max_abs": max(checks),
            "fluorophores": fluor_names, "variants": result}


def conditioning(fluorophores, afs) -> dict:
    reference = np.column_stack([fluorophores, afs.T])
    operator = est.ols_operator(reference)
    return {
        "condition_number": est.condition_number(reference),
        "fluorophore_condition_number": est.condition_number(fluorophores),
        "max_af_fluorophore_cosine": float(est.cosine_matrix(afs, fluorophores.T).max()),
        "unit_noise_variance": np.diag(operator @ operator.T).tolist(),
    }


def single_stain_spreading(production, options, fluorophores, afs, solver) -> dict:
    """In-sample (the positives built the spectra): per stain and off-target fluorophore,
    sqrt(max(SD²(positives) − SD²(negatives), 0)) / sqrt(median primary abundance)."""
    detectors = production["events"]["detectors"]
    af = Acquisition(control_path(production, production["events"]["autofluorescence"]["filename"]), detectors, options["fsc"], options["ssc"])
    values = []
    for index, stain in enumerate(production["events"]["single_stains"]):
        control = Acquisition(control_path(production, stain["filename"]), detectors, options["fsc"], options["ssc"])
        positive, _ = unmix_variant(control.detectors[stain["positive"]], fluorophores, afs, solver)
        negative, _ = unmix_variant(af.detectors[stain["matched_negative"]], fluorophores, afs, solver)
        primary = np.median(positive[:, index])
        for j in range(fluorophores.shape[1]):
            if j != index and primary > 0:
                excess = est.robust_sd(positive[:, j]) ** 2 - est.robust_sd(negative[:, j]) ** 2
                values.append(np.sqrt(max(excess, 0.0)) / np.sqrt(primary))
    return {"label": "in-sample", "median": float(np.median(values)), "max": float(np.max(values))}


def downstream(Y, scatter, gate_scatter, fluorophores, fitted, names, fluor_count, r_reference=None, fluor_names=()) -> dict:
    """The stained sample unmixed with each variant, against the production variant."""
    from scipy.spatial import Delaunay
    inside = Delaunay(gate_scatter).find_simplex(scatter) >= 0
    signal = np.linalg.norm(Y, axis=1)
    base_X, _ = unmix_variant(Y, fluorophores, fitted[est.BASELINE_ARM], "joint")
    base_sd = np.array([est.robust_sd(base_X[:, j]) for j in range(fluor_count)])
    result = {"outside_af_gate_fraction": float(1.0 - inside.mean())}
    for variant, (arm, solver) in names.items():
        if fitted[arm] is None:
            continue
        X, residuals = unmix_variant(Y, fluorophores, fitted[arm], solver)
        relative = residuals / np.where(signal > 0, signal, 1.0)
        delta = np.sqrt(np.mean((X[:, :fluor_count] - base_X[:, :fluor_count]) ** 2, axis=0)) / base_sd
        entry = {
            "rms_delta_over_robust_sd": delta.tolist(),
            "max_rms_delta_over_robust_sd": float(delta.max()),
            "negative_fraction": float(np.mean(X[:, :fluor_count] < 0)),
            "relative_residual_rms": float(np.sqrt(np.sum(residuals ** 2) / np.sum(Y ** 2))),
            "median_relative_residual_inside_af_gate": float(np.median(relative[inside])),
            "median_relative_residual_outside_af_gate": float(np.median(relative[~inside])) if (~inside).any() else None,
        }
        if r_reference is not None:
            entry["correlation_to_r"] = {
                "label": "AutoSpectral's AF spectrum is itself unstable between random draws; not truth",
                "pearson": [float(np.corrcoef(X[:, j], r_reference[f"r_{name}"])[0, 1]) for j, name in enumerate(fluor_names)]}
        result[variant] = entry
    return result


def truth_metrics(Y, fluorophores, fitted, names, fluor_count, truth: dict) -> dict:
    """Synthetic only: AF recovery and abundance error against the known truth."""
    result = {}
    sources = truth["af_spectra"]
    for variant, (arm, solver) in names.items():
        if fitted[arm] is None:
            continue
        X, _ = unmix_variant(Y, fluorophores, fitted[arm], solver)
        error = X[:, :fluor_count] - truth["abundance"]
        entry = {
            "af_largest_principal_angle_degrees": float(np.degrees(est.largest_principal_angle(fitted[arm], sources))),
            "abundance_rmse": np.sqrt(np.mean(error ** 2, axis=0)).tolist(),
            "abundance_bias": error.mean(axis=0).tolist(),
        }
        entry["median_abundance_rmse"] = float(np.median(entry["abundance_rmse"]))
        if truth["weak"].any():
            entry["weak_population_auc"] = {
                truth["names"][j]: est.auc(X[truth["weak"][:, j], j],
                                           X[(truth["abundance"][:, j] == 0) & ~truth["weak"][:, j], j])
                for j in range(fluor_count) if truth["weak"][:, j].any()}
        result[variant] = entry
    return result


def check_production_unmixing(production, Y, fluorophores, af) -> float:
    """The baseline variant must reproduce the production unmixed FCS within f32 rounding."""
    data, columns = read_fcs(production["unmixed"])
    rust = data[:, [columns.index(name) for name in production["names"]]]
    X, _ = unmix_variant(Y, fluorophores, af, "joint")
    operator = est.ols_operator(np.column_stack([fluorophores, af.T]))
    scale = np.abs(Y) @ np.abs(operator).T
    excess = np.abs(X - rust) - (2.0 ** -24 * np.abs(X) + 1e-9 * scale)
    if excess.max() > 0:
        raise AssertionError(f"production unmixing not reproduced: excess {excess.max()}")
    return float(np.max(np.abs(X - rust) / np.maximum(scale, 1e-300)))


def evaluate_dataset(binary, source: Source, output: Path, options, label, kind, settings, truth=None, r_reference=None) -> dict:
    output.mkdir(parents=True)
    sample = source.samples[0]
    if settings["sample_step"] > 1:
        events = len(read_fcs(sample)[0])
        subset(sample, output / "sample.fcs", np.arange(0, events, settings["sample_step"]))
        sample = output / "sample.fcs"
    full = run_production(binary, source, output / "full", options, sample=sample)
    oracle = oracle_check(full, options)
    require_oracle(oracle)
    detectors = full["events"]["detectors"]
    fluor_names = full["names"][:-1]
    fluorophores = full["reference"][:, :-1]
    af_file = control_path(full, full["events"]["autofluorescence"]["filename"])
    af = Acquisition(af_file, detectors, options["fsc"], options["ssc"])
    gate = np.asarray(full["events"]["autofluorescence"]["retained"], dtype=np.int64)
    Y_gate, S_gate = af.detectors[gate], af.scatter[gate]
    arms = {**est.SINGLE_AF_ARMS, **est.MULTI_AF_ARMS}
    full_fits = {name: fit_arm(arm, Y_gate, S_gate) for name, arm in arms.items()}
    fitted = {name: fit[0] for name, fit in full_fits.items()}
    names = variants(arms)
    sample_events = Acquisition(sample, detectors, options["fsc"], options["ssc"])
    reproduced = check_production_unmixing(full, sample_events.detectors, fluorophores, fitted[est.BASELINE_ARM])
    stable = stability(arms, Y_gate, S_gate, full_fits, settings["stability_seeds"], (1_000, 10_000))
    peak = full_fits[est.BASELINE_ARM][1]["peak_detector"]
    runner_up = est.first_argmax(est.mean_spectrum(Y_gate), peak)
    held = held_out(binary, source, output / "held_out", options, af, af_file, gate, arms,
                    settings["held_out_seeds"], settings["spikes"], sample, fluor_names)
    base_condition = est.condition_number(np.column_stack([fluorophores, fitted[est.BASELINE_ARM].T]))
    per_arm = {}
    for name, (spectra, info) in full_fits.items():
        entry = {"status": "ok" if spectra is not None else "failed",
                 "info": {k: v for k, v in info.items() if k != "labels"},
                 "cosine_to_production": float(est.matched_cosines(spectra, fitted[est.BASELINE_ARM]).min()) if spectra is not None else None,
                 "stability": stable[name]}
        if spectra is not None:
            entry["spectra"] = spectra.tolist()
            entry["conditioning"] = conditioning(fluorophores, spectra)
            entry["conditioning"]["condition_ratio_to_production"] = entry["conditioning"]["condition_number"] / base_condition
        if name in est.MULTI_AF_ARMS:
            split = stable[name]["split_half_min_component_cosine"]
            entry["identifiability"] = (est.identifiability(spectra, info["events_per_component"], fluorophores, base_condition,
                                                            split if split is not None else -1.0)
                                        if spectra is not None else {"identifiable": False, "checks": {"finite_spectra": False}})
            if name.startswith("spectral_clusters") and spectra is not None:
                from sklearn.metrics import adjusted_rand_score
                labels = [fit_arm(arms[name], Y_gate, S_gate, seed=seed)[1].get("labels") for seed in est.CLUSTER_SEEDS]
                entry["init_seed_label_agreement_min_ari"] = float(min(adjusted_rand_score(labels[0], other) for other in labels[1:] if other is not None))
        per_arm[name] = entry
    spreading = {variant: single_stain_spreading(full, options, fluorophores, fitted[arm], solver)
                 for variant, (arm, solver) in names.items() if fitted[arm] is not None}
    result = {
        "label": label, "kind": kind, "oracle": oracle,
        "production_af_peak_sensitivity": full["run"]["reference_diagnostics"].get("af_peak_sensitivity"),
        "af_gate": {"retained": int(gate.size), "total": int(af.detectors.shape[0])},
        "peak_margin_bootstrap": peak_margin_bootstrap(Y_gate, peak, runner_up),
        "production_unmixing_reproduced_max_relative": reproduced,
        "sample_events": int(sample_events.detectors.shape[0]),
        "arms": per_arm,
        "held_out_unstained": held,
        "single_stain_spreading": spreading,
        "downstream": downstream(sample_events.detectors, sample_events.scatter, S_gate, fluorophores, fitted, names,
                                 len(fluor_names), r_reference, fluor_names),
    }
    if truth is not None:
        result["truth"] = truth_metrics(sample_events.detectors, fluorophores, fitted, names, len(fluor_names), truth)
    return result


# --------------------------------------------------------------------------- decision
def criteria(dataset: dict, variant: str) -> dict:
    """Each DECISION_RULE criterion of `variant` on one dataset: True when met."""
    arm = variant.split("+")[0]
    rule = DECISION_RULE
    kind = dataset["kind"]
    held = dataset["held_out_unstained"]["variants"].get(variant, {})
    base = dataset["arms"][rule["baseline"]]
    entry = dataset["arms"][arm]
    if held.get("status") != "ok" or entry["status"] != "ok":
        return {"evaluated": False}
    base_split = base["stability"]["split_half_min_component_cosine"] or 0.0
    split = entry["stability"]["split_half_min_component_cosine"]
    result = {
        "evaluated": True,
        "oracle": dataset["oracle"]["status"] == "passed",
        "held_out_median_robust_sd_ratio": held["median_robust_sd_ratio"] <= rule["held_out_median_robust_sd_ratio_max"][kind],
        "held_out_p99_ratio": held["max_p99_ratio"] <= rule["held_out_p99_ratio_max_any_component"],
        "split_half_min_cosine": split is not None and split >= max(base_split, rule["split_half_min_cosine_floor"]),
        "condition_ratio": entry["conditioning"]["condition_ratio_to_production"] <= rule["condition_ratio_max"],
        "af_fluorophore_cosine_increase": entry["conditioning"]["max_af_fluorophore_cosine"] - base["conditioning"]["max_af_fluorophore_cosine"] <= rule["af_fluorophore_cosine_increase_max"],
    }
    if arm in est.MULTI_AF_ARMS:
        result["identifiable"] = bool(entry["identifiability"]["identifiable"])
    return result


def decide(datasets: list[dict]) -> dict:
    """Eligibility of every non-baseline variant over all datasets; never automatic adoption."""
    rule = DECISION_RULE
    real = [d for d in datasets if d["kind"] == "real"]
    names = sorted({v for d in datasets for v in d["held_out_unstained"]["variants"]} - {rule["baseline"]})
    decision = {}
    for variant in names:
        failed = []
        for dataset in datasets:
            checks = criteria(dataset, variant)
            if not checks.get("evaluated"):
                failed.append(f"{dataset['label']}: not evaluated")
                continue
            failed += [f"{dataset['label']}: {name}" for name, ok in checks.items() if name != "evaluated" and not ok]
        if len(real) < rule["independent_real_datasets_min"]:
            failed.append(f"{len(real)} independent real dataset(s) < {rule['independent_real_datasets_min']}")
        decision[variant] = {"eligible": not failed, "failed_criteria": failed,
                             "reason": "all predeclared criteria met" if not failed else f"{len(failed)} criteria not met"}
    return decision


# --------------------------------------------------------------------------- reports
def clean(value):
    """JSON-safe: NumPy scalars to Python, non-finite floats to None."""
    if isinstance(value, dict):
        return {str(k): clean(v) for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return [clean(v) for v in value]
    if isinstance(value, np.ndarray):
        return clean(value.tolist())
    if isinstance(value, (np.bool_,)):
        return bool(value)
    if isinstance(value, (np.integer,)):
        return int(value)
    if isinstance(value, (float, np.floating)):
        return float(value) if np.isfinite(value) else None
    return value


def provenance(binary: Path | None, arguments: dict, inputs: dict) -> dict:
    return {
        "binary_sha256": sha256(binary) if binary else None,
        "scripts": {name: sha256(ROOT / name) for name in SCRIPTS if (ROOT / name).exists()},
        "inputs": inputs,
        "numpy": np.__version__,
        "scipy": scipy.__version__,
        "arguments": {key: str(value) for key, value in arguments.items()},
    }


def write_report(output: Path, report: dict, name: str = "report.json"):
    (output / name).write_text(json.dumps(clean(report), indent=2, allow_nan=False) + "\n")


def settings_of(args) -> dict:
    return {"stability_seeds": list(range(1, args.stability_seeds + 1)),
            "held_out_seeds": list(range(1, args.held_out_seeds + 1)),
            "spikes": [float(v) for v in args.spikes.split(",")],
            "sample_step": args.sample_step}


def mode_oracle(args, report) -> None:
    source = Source(args.source)
    options = {"fsc": args.fsc, "ssc": args.ssc, "quantile_gate": args.quantile_gate}
    report["provenance"] = provenance(args.binary, vars(args), source.hashes())
    production = run_production(args.binary, source, args.output / "production", options)
    report["oracle"] = oracle_check(production, options)
    require_oracle(report["oracle"])


def mode_synthetic(args, report) -> None:
    import af_scenarios
    options = {"fsc": "FSC-A", "ssc": "SSC-A", "quantile_gate": False}
    settings = settings_of(args)
    report["provenance"] = provenance(args.binary, vars(args), {})
    report["datasets"] = []
    for scenario in args.scenarios.split(","):
        for seed in [int(s) for s in args.seeds.split(",")]:
            label = f"{scenario}/{seed}"
            directory = args.output / "inputs" / scenario / str(seed)
            generated = af_scenarios.write_scenario(directory, scenario, seed)
            truth_dir = directory / "truth"
            names = list(pd.read_csv(truth_dir / "spectra.csv", index_col=0).index)
            abundance = pd.read_csv(truth_dir / "sample_abundance.csv")
            truth = {"names": names, "abundance": abundance[names].to_numpy(),
                     "af_spectra": pd.read_csv(truth_dir / "af_spectra.csv", index_col=0).to_numpy(),
                     "weak": pd.read_csv(truth_dir / "weak.csv")[names].to_numpy()}
            source = Source(directory)
            result = evaluate_dataset(args.binary, source, args.output / "runs" / scenario / str(seed), options,
                                      label, scenario, settings, truth=truth)
            result["scenario"] = generated
            result["input_sha256"] = source.hashes()
            report["datasets"].append(result)
            write_report(args.output, report, "progress.json")
    report["decision"] = decide(report["datasets"])
    report["production_change_recommended"] = any(d["eligible"] for d in report["decision"].values())


def mode_real(args, report) -> None:
    source = Source(args.source)
    options = {"fsc": args.fsc, "ssc": args.ssc, "quantile_gate": args.quantile_gate}
    settings = settings_of(args)
    report["provenance"] = provenance(args.binary, vars(args), source.hashes())
    r_reference = None
    candidates = sorted(args.source.glob("r_unmixed_*.csv"))
    if candidates and settings["sample_step"] == 10:
        # parity/real/real.R unmixes every 10th sample event: the same rows.
        r_reference = pd.read_csv(candidates[0])
    result = evaluate_dataset(args.binary, source, args.output / "runs", options, args.label, "real", settings,
                              r_reference=r_reference)
    report["datasets"] = [result]
    report["decision"] = decide(report["datasets"])
    report["production_change_recommended"] = any(d["eligible"] for d in report["decision"].values())


# --------------------------------------------------------------------------- table
def _fmt(value, digits=3):
    return "–" if value is None else f"{value:.{digits}f}"


def mode_table(args, report) -> None:
    datasets = []
    for path in args.report:
        loaded = json.loads(path.read_text())
        if loaded.get("decision_rule_sha256") != DECISION_RULE_SHA256:
            raise ValueError(f"{path}: produced under a different decision rule")
        if loaded.get("status") != "completed":
            raise ValueError(f"{path}: status {loaded.get('status')}")
        datasets += loaded["datasets"]
    report["datasets_included"] = [d["label"] for d in datasets]
    report["decision"] = decide(datasets)
    report["production_change_recommended"] = any(d["eligible"] for d in report["decision"].values())
    lines = ["| dataset | variant | cos to prod. | split-half min | switch 10k | held-out SD ratio (median) | p99 ratio | κ | max AF↔fluor cos | downstream max Δ/SD | resid. outside gate | eligible |",
             "|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|"]
    for dataset in datasets:
        for variant, (arm, solver) in variants({**est.SINGLE_AF_ARMS, **est.MULTI_AF_ARMS}).items():
            entry = dataset["arms"].get(arm, {})
            held = dataset["held_out_unstained"]["variants"].get(variant, {})
            down = dataset["downstream"].get(variant, {})
            if entry.get("status") != "ok":
                lines.append(f"| {dataset['label']} | {variant} | failed | | | | | | | | | no |")
                continue
            switch = entry["stability"]["subsample"].get("10000", {}).get("peak_switch_rate")
            eligible = "–" if variant == est.BASELINE_ARM else ("yes" if report["decision"][variant]["eligible"] else "no")
            lines.append("| " + " | ".join([
                dataset["label"], variant, _fmt(entry["cosine_to_production"], 4),
                _fmt(entry["stability"]["split_half_min_component_cosine"], 4), _fmt(switch, 2),
                _fmt(held.get("median_robust_sd_ratio")), _fmt(held.get("max_p99_ratio")),
                _fmt(entry["conditioning"]["condition_number"], 2), _fmt(entry["conditioning"]["max_af_fluorophore_cosine"]),
                _fmt(down.get("max_rms_delta_over_robust_sd")), _fmt(down.get("median_relative_residual_outside_af_gate")),
                eligible]) + " |")
    table = "\n".join(lines) + "\n"
    (args.output / "table.md").write_text(table)
    print(table)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("mode", choices=["oracle", "synthetic", "real", "table"])
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--source", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--report", type=Path, action="append", default=[])
    parser.add_argument("--label", default="aurora_pbmc_spectral")
    parser.add_argument("--fsc", default="FSC-A")
    parser.add_argument("--ssc", default="SSC-A")
    parser.add_argument("--quantile-gate", action="store_true")
    parser.add_argument("--scenarios", default="single_af,two_af_near_tie,minority_af_scatter_linked,minority_af_scatter_independent,weak_positive")
    parser.add_argument("--seeds", default="20261101,20261102,20261103")
    parser.add_argument("--stability-seeds", type=int, default=20)
    parser.add_argument("--held-out-seeds", type=int, default=5)
    parser.add_argument("--spikes", default="1,2,3,5")
    parser.add_argument("--sample-step", type=int, default=1)
    args = parser.parse_args()
    if args.mode != "table" and args.binary is None:
        parser.error("--binary is required")
    if args.mode in ("oracle", "real") and args.source is None:
        parser.error("--source is required")
    args.output.mkdir(parents=True, exist_ok=False)
    report = {"schema_version": SCHEMA_VERSION, "experiment": "af_reference_benchmark", "mode": args.mode,
              "status": "failed", "decision_rule": DECISION_RULE, "decision_rule_sha256": DECISION_RULE_SHA256}
    started = time.monotonic()
    try:
        {"oracle": mode_oracle, "synthetic": mode_synthetic, "real": mode_real, "table": mode_table}[args.mode](args, report)
        report["status"] = "completed"
    except Exception as error:
        report["error"] = f"{type(error).__name__}: {error}"
        raise
    finally:
        report["wall_seconds"] = time.monotonic() - started
        write_report(args.output, report)


if __name__ == "__main__":
    main()
