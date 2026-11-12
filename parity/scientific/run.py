#!/usr/bin/env python3
"""Seeded robustness experiments on production CLI outputs and independent truth.

Outputs always live outside golden fixtures. Any missing input/failed CLI or
nonfinite metric fails the run and leaves a machine-readable failure report.
"""
from __future__ import annotations
import argparse
import csv
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

import numpy as np
import pandas as pd
import pyarrow.parquet as pq

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "parity/gen"))
from fcsio import read_fcs, write_fcs
from generate import gen_autospectral, gen_autospill
from cytoscan import write_cohort, CHANNELS
from metrics import coefficient_perturbations, empirical_spreading, retention, spectral_metrics, wilson_interval, solver_study, gaussian_reduction_study, unmixing_spreading
from datasets import sha256


def invoke(binary, args, log):
    with log.open("w") as handle:
        subprocess.run([str(binary), *map(str, args)], check=True, stdout=handle, stderr=handle,
                       env={**os.environ, "RAYON_NUM_THREADS": "4"})


def references(output):
    path = output / "references/autospectral-references.json"
    document = json.loads(path.read_text())
    return np.array([x["spectrum"] for x in document["components"]]).T


def control_sensitivity(binary, root, spectral, repetitions, events):
    dataset = root / "inputs"
    dataset.mkdir()
    if spectral:
        gen_autospectral(dataset, n_events=events, n_sample=2000)
    else:
        gen_autospill(dataset, n_events=events)
    controls = pd.read_csv(dataset / "controls.csv")
    originals = {name: read_fcs(dataset / name) for name in controls["filename"]}
    estimates = []
    for i in range(repetitions):
        with tempfile.TemporaryDirectory(dir=root) as temporary:
            directory = Path(temporary)
            controls.to_csv(directory / "controls.csv", index=False)
            for name, (values, channels) in originals.items():
                # Independent streams per control; bootstrap resamples events
                # and reruns production gating and estimation end to end.
                seed = 42 + i + int(hashlib.sha256(name.encode()).hexdigest()[:8], 16)
                rng = np.random.default_rng(seed)
                ids = np.sort(rng.integers(0, len(values), len(values))) if i else np.arange(len(values))
                write_fcs(directory / name, values[ids], channels)
            output = directory / "result"
            if spectral:
                samples = pd.read_csv(dataset / "samples.csv")
                for name in samples["filename"]:
                    values, channels = read_fcs(dataset / name)
                    write_fcs(directory / name, values, channels)
                samples.to_csv(directory / "samples.csv", index=False)
                invoke(binary, ["autospectral", "--controls", directory / "controls.csv", "--samples", directory / "samples.csv", "--input-dir", directory, "--output", output], directory / "run.log")
                estimates.append(references(output))
                if i == 0:
                    diagnostics = json.loads((output/"diagnostics/run.json").read_text())["reference_diagnostics"]
                    oracle = spectral_metrics(estimates[-1])
                    np.testing.assert_allclose(diagnostics["spectral_cosine"],oracle["spectral_cosine"],rtol=1e-12,atol=1e-12)
                    np.testing.assert_allclose(diagnostics["unit_noise_covariance"],oracle["unit_noise_covariance"],rtol=1e-10,atol=1e-12)
            else:
                invoke(binary, ["autospill", "--controls", directory / "controls.csv", "--input-dir", directory, "--output", output.with_suffix(".csv"), "--matrix-kind", "spillover"], directory / "run.log")
                estimates.append(pd.read_csv(output.with_suffix(".csv"), index_col=0).to_numpy())
    stack = np.asarray(estimates)
    result = {"resampling_unit": "events; gating and fit recomputed", "replicates": repetitions-1,
              "independent_control_uncertainty": {"status": "not_assessed", "reason": "one acquisition per dye"},
              "max_absolute_change": float(np.max(np.abs(stack-stack[0]))),
              "confidence_interval_status": "assessed" if repetitions >= 33 else "not_assessed"}
    if repetitions >= 33:
        result["percentile_ci95"] = np.quantile(stack[1:], [.025, .975], axis=0).tolist()
        result["confidence_interval_quality"] = "full" if repetitions >= 1001 else "exploratory; fewer than 1000 resamples"
    if spectral:
        result["baseline"] = spectral_metrics(stack[0])
        result["af_cosine_to_baseline"] = [float(np.dot(a[:, -1], stack[0, :, -1]) / (np.linalg.norm(a[:, -1])*np.linalg.norm(stack[0, :, -1]))) for a in stack]
        result["alternative_solver_feasibility"] = solver_study()
        truth = pd.read_csv(dataset/"truth/spectra.csv",index_col=0).to_numpy().T
        result["independent_unmixing_spreading"] = unmixing_spreading(stack[0],truth)
    else:
        truth = pd.read_csv(dataset / "truth/spillover.csv", index_col=0).to_numpy()
        result["error_against_truth"] = float(np.max(np.abs(stack[0]-truth)))
        independent=[]
        for i in range(3):
            acquisition=root/f"independent_{i}"; acquisition.mkdir()
            gen_autospill(acquisition,n_events=events,seed=20262022+i)
            estimate=acquisition/"spillover.csv"
            invoke(binary,["autospill","--controls",acquisition/"controls.csv","--input-dir",acquisition,"--output",estimate,"--matrix-kind","spillover"],acquisition/"run.log")
            independent.append(pd.read_csv(estimate,index_col=0).to_numpy())
        result["independent_control_uncertainty"]={"status":"synthetic_independent_acquisitions","acquisitions":3,"seeds":[20262022+i for i in range(3)],"max_coefficient_range":float(np.ptp(independent,axis=0).max()),"real_repeat_controls":{"status":"not_assessed","reason":"no matched experimental repeat acquisitions in pinned controls"}}
        invoke(binary,["autospread","--controls",dataset/"controls.csv","--input-dir",dataset,"--output",root/"autospread.csv"],root/"autospread.log")
        spread=pd.read_csv(root/"autospread.csv",index_col=0).to_numpy()
        validation=np.random.default_rng(101)
        empirical=[]
        for dye in range(len(truth)):
            positive=np.zeros((10000,len(truth))); positive[:,dye]=10000.
            positive=positive@truth
            negative=validation.normal(0,60,positive.shape)
            positive=validation.poisson(positive/2)*2+validation.normal(0,60,positive.shape)
            compensation=np.linalg.inv(stack[0])
            empirical.append(empirical_spreading(positive@compensation,negative@compensation,10000.)["spread"])
        off_diagonal=~np.eye(len(truth),dtype=bool)
        result["autospread_independent_validation"]={"orientation":"rows=dyes, columns=detectors","estimated_units":"slope of sqrt_like(secondary variance excess) against sqrt_like(primary signal)","empirical_units":"compensated secondary signal units / sqrt(primary signal units)","units_note":"asymptotically comparable at high signal; sqrt_like(x)=sign(x)*(sqrt(abs(x)+1)-1) is not exactly homogeneous","estimator_domain":"raw detector spread surrogate","validation_domain":"compensated independent controls","estimated":spread.tolist(),"empirical":empirical,"max_off_diagonal_absolute_difference":float(np.abs(spread-np.array(empirical))[off_diagonal].max()),"diagonal":"structural zero excluded from comparison","automatic_compensation_change":False}
        if repetitions >= 33:
            lower,upper=np.asarray(result["percentile_ci95"])
            off_diagonal=~np.eye(len(truth),dtype=bool)
            result["truth_coefficients_inside_ci_fraction"]=float(np.mean(((truth>=lower)&(truth<=upper))[off_diagonal]))
        # Independent events for propagation: x S is compensated with a
        # perturbed S, without using SSI or changing a production matrix.
        rng = np.random.default_rng(42)
        latent = rng.lognormal(4, 1, (10000, len(truth)))
        observed = latent @ truth
        result["coefficient_perturbation"] = {}
        for delta in (.0001, .001, .01):
            perturbed = truth.copy()
            perturbed[0, 1] += delta
            residual = observed @ np.linalg.inv(perturbed) - latent
            result["coefficient_perturbation"][str(delta)] = {"rmse": float(np.sqrt(np.mean(residual**2))), "max_abs": float(np.abs(residual).max())}
        # Every off-diagonal coefficient, with the exact first-order bound.
        perturbations = coefficient_perturbations(truth, latent)
        for cell, by_delta in perturbations.items():
            for delta, entry in by_delta.items():
                if entry["first_order_relative_error"] > entry["bound"] + 1e-12:
                    raise AssertionError(f"first-order bound violated at {cell} delta {delta}")
        result["coefficient_perturbation_all"] = perturbations
    return result


def novelty(binary, root, events, sizes):
    data = root / "inputs"
    write_cohort(data, 42, 24, max(sizes), events, {})
    reference = sorted((data / "reference").glob("*.fcs"))
    test = sorted((data / "test").glob("*.fcs"))
    results = []
    for seed in (1, 7, 42, 101, 2026):
        order = np.random.default_rng(seed).permutation(len(reference))
        for n in sizes:
            for method in ("summary", "quantiles", "emd", "binning", "fingerprint"):
                output = root / f"{seed}-{n}-{method}"
                args = ["cytoscan", "--output", output, "--debug-dir", output/"debug", "--channels", ",".join(CHANNELS), "--method", method, "--flag-method", "both"]
                for path in test: args += ["--input", path]
                for i in order[:n]: args += ["--reference", reference[i]]
                log = root / f"{seed}-{n}-{method}.log"
                try:
                    invoke(binary, args, log)
                except subprocess.CalledProcessError:
                    # Fit failures are observations, never removed from the
                    # experiment's denominator or counted as non-anomalies.
                    results.append({"seed": seed, "n_reference": n, "method": method, "status": "fit_failed", "log": str(log)})
                    continue
                summary = pd.read_csv(output / "cytoscan_summary.csv")
                flags = summary["is_novelty"].astype(str).str.lower().isin(["true", "1"])
                hits = int(flags.sum())
                outlier_hits = int(summary["is_outlier"].astype(str).str.lower().isin(["true", "1"]).sum())
                reference_features = pd.read_csv(output / "debug/reference_features.csv").select_dtypes(include="number").to_numpy()
                test_features = pd.read_csv(output / "cytoscan_features.csv").select_dtypes(include="number").to_numpy()
                results.append({"seed": seed, "n_reference": n, "feature_dimension":reference_features.shape[1], "method": method, "status": "assessed", "false_positives": hits, "normal_files": len(flags), "fpr": hits/len(flags), "fpr_ci95": wilson_interval(hits, len(flags)), "outlier_false_positives":outlier_hits,"outlier_fpr":outlier_hits/len(flags),"outlier_ci95":wilson_interval(outlier_hits,len(flags)),"outlier_fit_domain":"within the normal test cohort; not a held-out forest", "research_only_covariance": gaussian_reduction_study(reference_features,test_features)})
    return {"null_cohort": "independent synthetic normal acquisitions", "results": results,
            "seed_scope": "reference membership at the default algorithm seed 42",
            "production_seed_stability": production_seed_stability(binary, root, test, reference[:32])}


ALGORITHM_SEEDS = (1, 7, 42, 101, 2026)


def production_seed_stability(binary, root, test, reference):
    """Vary only `cytoscan --seed` (event subsample and isolation trees) with
    fixed files and reference membership. Reports per-file outlier-score ranges
    and whether each flag agrees across seeds; nothing is gated."""
    methods = {}
    for method in ("summary", "quantiles"):
        runs = {}
        for seed in ALGORITHM_SEEDS:
            output = root / f"algorithm-seed-{seed}-{method}"
            args = ["cytoscan", "--output", output, "--channels", ",".join(CHANNELS), "--method", method, "--flag-method", "both", "--seed", str(seed)]
            for path in test: args += ["--input", path]
            for path in reference: args += ["--reference", path]
            invoke(binary, args, root / f"algorithm-seed-{seed}-{method}.log")
            runs[seed] = pd.read_csv(output / "cytoscan_summary.csv").set_index("filename")
        files = sorted(runs[ALGORITHM_SEEDS[0]].index)
        flag = lambda frame, column: frame[column].astype(str).str.lower().isin(["true", "1"])
        per_file = []
        for name in files:
            scores = [float(runs[seed].loc[name, "outlier_score"]) for seed in ALGORITHM_SEEDS]
            outlier = [bool(flag(runs[seed], "is_outlier")[name]) for seed in ALGORITHM_SEEDS]
            novelty = [bool(flag(runs[seed], "is_novelty")[name]) for seed in ALGORITHM_SEEDS]
            per_file.append({"filename": name, "outlier_score_min": min(scores), "outlier_score_max": max(scores),
                             "outlier_score_range": max(scores) - min(scores),
                             "outlier_flags": outlier, "novelty_flags": novelty,
                             "outlier_stable": len(set(outlier)) == 1, "novelty_stable": len(set(novelty)) == 1})
        methods[method] = {"files": per_file,
                           "max_outlier_score_range": max(f["outlier_score_range"] for f in per_file),
                           "outlier_flag_stable_fraction": float(np.mean([f["outlier_stable"] for f in per_file])),
                           "novelty_flag_stable_fraction": float(np.mean([f["novelty_stable"] for f in per_file]))}
    return {"seeds": list(ALGORITHM_SEEDS), "reference_files": len(reference), "test_files": len(test),
            "scope": "algorithm seed only: event subsample and isolation trees; files and reference membership fixed",
            "methods": methods}


def real_retention(binary, root, registry, cache):
    from datasets import verify
    lock = json.loads(registry.read_text())
    if not lock.get("complete"): raise ValueError("incomplete required registry")
    verify(lock, cache)
    import flowio
    reports = []
    for item in lock["files"]:
        source = cache / item["path"]
        if source.suffix.lower() != ".fcs": continue
        flow = flowio.FlowData(str(source))
        names = [flow.text[f"p{i+1}n"] for i in range(flow.channel_count)]
        labels = [flow.text.get(f"p{i+1}s", "") for i in range(flow.channel_count)]
        label_columns = [i for i, name in enumerate(names) if any(word in (name+" "+labels[i]).lower() for word in ("celltype", "cell_type", "cell type", "cell_id", "cell identity", "label", "omiqfilter"))]
        if len(label_columns) != 1: raise ValueError(f"ambiguous/missing population labels: {source}: {list(zip(names, labels))}")
        values = np.asarray(flow.events).reshape(-1, flow.channel_count)
        # Original_ID is one-based acquisition lineage, not the current file's
        # row number. Join masks by ID when validating an already subset file.
        source_ids = values[:, names.index("Original_ID")]-1 if "Original_ID" in names else np.arange(len(values))
        if not np.isfinite(source_ids).all() or np.any(source_ids < 0) or np.any(source_ids != np.floor(source_ids)) or len(np.unique(source_ids)) != len(source_ids):
            raise ValueError("invalid source acquisition lineage")
        source_rows = {int(value): row for row,value in enumerate(source_ids)}
        population = values[:, label_columns[0]]
        if names[label_columns[0]] == "OmiqFilter":
            vocabulary = flow.text["omiqfilternames"].split(",")
            if not np.isfinite(population).all() or np.any(population != np.floor(population)) or np.any(population < 0) or np.any(population >= len(vocabulary)):
                raise ValueError("invalid published OMIQ population codes")
            population = np.array(vocabulary)[population.astype(int)]
        channels = [name for i,name in enumerate(names) if i != label_columns[0] and any(word in labels[i] for word in ("CD", "IgD", "TCR"))]
        if not channels: raise ValueError("missing declared biological marker labels")
        case = root / Path(item["path"]).parent.name / source.stem
        case.mkdir(parents=True)
        output = case / "cleaned.fcs"
        invoke(binary, ["peacoqc", "--input", source, "--output", output, "--channels", ",".join(channels)], case / "peacoqc.log")
        table = pq.read_table(output.with_suffix(".masks.parquet")).to_pandas()
        keep = np.zeros(len(population), dtype=bool)
        raw_ids = table["source_event_id"].to_numpy(dtype=np.uint64)
        if len(np.unique(raw_ids)) != len(raw_ids) or any(int(i) not in source_rows for i in raw_ids):
            raise ValueError("invalid event lineage")
        ids = np.array([source_rows[int(i)] for i in raw_ids],dtype=np.int64)
        keep[ids] = table["keep"].to_numpy(dtype=bool)
        stages=[]; assessments=[]
        for column in ("margin_pass","peacoqc_pass"):
            if column not in table or not table[column].notna().any():
                assessments.append({"stage":column,"status":"not_assessed","reason":"mask has no assessed values"})
                continue
            passed=np.zeros(len(population),dtype=bool)
            passed[ids]=table[column].fillna(True).to_numpy(dtype=bool)
            stages.append((column,passed))
            assessments.append({"stage":column,"status":"assessed"})
        stages.append(("combined",keep))
        reports.append({"input": item, "retention": retention(population, stages), "assessments":assessments,
                        "label_alignment":"joined by original source event ID",
                        "technical_removal_accuracy": {"status": "not_assessed", "reason": "published labels identify populations, not technical artefacts"},
                        "clean": {"status": "not_assessed", "reason": "CyTOF has no validated FSC/SSC clean gate"}})
        output.unlink()
        del flow, values
        with (root/"retention.jsonl").open("a") as journal:
            journal.write(json.dumps(reports[-1],allow_nan=False)+"\n")
        (root/"progress.json").write_text(json.dumps({"completed_files":len(reports),"last_input":item["path"]},indent=2)+"\n")
    return {"files": reports}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("suite", choices=["spectral", "compensation", "novelty", "retention"])
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--replicates", type=int, default=1000)
    parser.add_argument("--events", type=int, default=5000)
    parser.add_argument("--sizes", default="4,8,16,32,64")
    parser.add_argument("--registry", type=Path)
    parser.add_argument("--cache", type=Path)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    report = {"schema_version": 1, "suite": args.suite, "seed": 42, "generator_seed":20261022,"generator_sha256":sha256(ROOT/"parity/gen/generate.py"),"runner_sha256":sha256(Path(__file__)),"binary_sha256": sha256(args.binary), "arguments": {k:str(v) for k,v in vars(args).items()}, "status": "failed"}
    started = time.monotonic()
    try:
        if args.suite in ("spectral", "compensation"):
            report["metrics"] = control_sensitivity(args.binary, args.output, args.suite == "spectral", args.replicates+1, args.events)
        elif args.suite == "novelty":
            report["metrics"] = novelty(args.binary, args.output, args.events, list(map(int, args.sizes.split(","))))
        else:
            if not args.registry or not args.cache: raise ValueError("retention requires pinned registry and cache")
            report["metrics"] = real_retention(args.binary, args.output, args.registry, args.cache)
        report["status"] = "completed"
    except Exception as error:
        report["error"] = str(error)
        raise
    finally:
        report["wall_seconds"] = time.monotonic()-started
        (args.output / "report.json").write_text(json.dumps(report, indent=2, allow_nan=False)+"\n")


if __name__ == "__main__": main()
