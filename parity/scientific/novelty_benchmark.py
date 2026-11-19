#!/usr/bin/env python3
"""Pre-registered benchmark of CytoScan novelty stability and calibration.

Independent synthetic normal cohorts (`parity/gen/cytoscan.py`) are drawn per
replicate from predeclared generator seeds (`novelty_scenarios.py`); every
arm is scored on held-out normal and anomalous test files from production
`cytiq cytoscan` runs. The frozen `DECISION_RULE` decides, on the
confirmation profile only, whether an arm may become an opt-in production
mode. Nothing is adopted automatically, real donors never yield an FPR point
estimate, and no fit failure is dropped from a denominator.

    novelty_benchmark.py run --profile ci|development|confirmation --binary BIN
        --events-bin EXAMPLE [--baseline-bin MAIN_BIN] [--jobs N] [--with-r-prior]
        [--rust-calibration] [--keep-inputs] [--keep-runs] --output NEW_DIR
    novelty_benchmark.py table --report DIR/report.json --output DIR
"""
from __future__ import annotations

import argparse
import concurrent.futures
import csv
import hashlib
import json
import math
import os
import platform
import shutil
import statistics
import subprocess
import sys
import threading
import time
from dataclasses import dataclass, field
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "parity/gen"))
from cytoscan import ANOMALIES, CHANNELS, Batch, write_files  # noqa: E402
from datasets import sha256  # noqa: E402
from metrics import wilson_interval  # noqa: E402
import novelty_arms as arms  # noqa: E402
import novelty_scenarios as sc  # noqa: E402

SCHEMA_VERSION = 1
BENCHMARK = "cytoscan_novelty_stability"

RULE_V1_SHA256 = "7ef5527ace5270c0abb12c260d423c7027198b19a4a3d3f658ce1d7d39c8f420"

DECISION_RULE = {
    "version": 2,
    "alpha": list(sc.ALPHA),
    "decision_profile": "confirmation",
    "baseline_arm": "baseline",
    "stability_seeds_evaluated": list(range(sc.STABILITY_SEEDS)),
    "ensemble_seed_groups": [list(range(5)), list(range(5, 10))],
    "folds_max": sc.FOLDS_MAX,
    "fold_rule": "index_mod_k_over_resolved_reference_order",
    "bootstrap": {"unit": "replicate", "replicates": 2000, "seed": sc.BOOTSTRAP_SEED,
                  "interval": "percentile", "level": 0.95},
    "decision_methods": ["summary", "quantiles"],
    "claims": {
        "calibration": {
            "arms": ["cross_conformal", "shrinkage_ledoit_wolf+cross_conformal"],
            "families": ["exchangeable", "dimension"],
            "must_abstain_below_reference": sc.MIN_CONFORMAL_REFERENCE,
            "reference_min": 20,
            "decision_rate_min": 0.95,
            "fpr_mean_max": 0.07,
            "fpr_upper95_max": 0.10,
            "novelty_flip_rate_max": 0.10,
            "detection": {"families": ["exchangeable"], "reference_min": 32,
                          "classes": list(ANOMALIES), "rate_min": 0.80},
        },
        "stability": {
            "arms": ["seed_ensemble_5", *sc.EVENT_ARMS],
            "families": ["exchangeable", "large_events"],
            "baseline_flip_rate_min_to_evaluate": 0.01,
            "flip_rate_ratio_max": 0.5,
            "fpr_paired_difference_upper95_max": 0.02,
            "detection_margin_vs_baseline": 0.05,
            "detection_reference_min": 32,
            "decision_rate_margin_vs_baseline": 0.02,
        },
    },
    "every_applicable_cell_must_pass": True,
    "equivalence_checks_must_pass": True,
    "research_only": ["cross_conformal_ensemble_5", "shrinkage_ledoit_wolf", "pca_gaussian",
                      "pca_gaussian+cross_conformal", "mclust_prior"],
    "descriptive_families": ["batch_shift", "aggregate_methods", "real_donors"],
    "scenarios": sc.SCENARIOS,
    "history": [{
        "from_version": 1, "from_sha256": RULE_V1_SHA256,
        "evidence": "development profile only (generator seeds 20261201-20261220); no confirmation seed had been used",
        "changes": [
            "calibration flip criterion: 'flip rate on normals <= baseline + 0.02' became 'flip rate on normals <= 0.10'; a baseline that flags every normal file has no flips, so the relative bound penalised any calibrated arm",
            "shrinkage_ledoit_wolf+cross_conformal moved from research_only to the calibration claim; it is now evaluated with per-seed decisions on the exchangeable and dimension families",
        ],
        "reason": "development showed every calibrated arm failing only the degenerate relative flip bound, and the mclust cross-conformal arm losing detection at n >= 32 where the Ledoit-Wolf arm kept it",
    }],
    "outcome": "an eligible calibration arm becomes an opt-in versioned --novelty-calibration mode and an eligible stability arm an opt-in flag; defaults stay byte-identical; otherwise a negative result is recorded",
}
DECISION_RULE_SHA256 = hashlib.sha256(json.dumps(DECISION_RULE, sort_keys=True).encode()).hexdigest()

SCRIPTS = ["parity/scientific/novelty_benchmark.py", "parity/scientific/novelty_arms.py",
           "parity/scientific/novelty_scenarios.py", "parity/scientific/novelty_prior.R",
           "parity/gen/cytoscan.py", "parity/gen/fcsio.py"]
FIT_FAILURES = ("novelty model fitting failed", "NoveltyFit", "fingerprint bin",
                "FingerprintBinTooSmall", "requires at least one aggregate event", "EmptyAggregate")
"""Failures a cohort can cause (no mixture fits, an empty fingerprint bin or
aggregate): observations of that cohort. Any other failure stops the run."""
EVENT_ARM_NAMES = tuple(sc.EVENT_ARMS)


# --------------------------------------------------------------------------- runs
@dataclass
class Run:
    """One production invocation, parsed. `seeds[j][filename]` holds seed j's row."""
    status: str
    reason: str | None = None
    seeds: dict = field(default_factory=dict)
    seed_status: dict = field(default_factory=dict)
    diagnostics: dict = field(default_factory=dict)
    parameters: dict = field(default_factory=dict)
    features: dict | None = None
    wall: float = 0.0


def _float(value: str) -> float | None:
    return None if value == "" else float(value)


def _bool(value: str) -> bool | None:
    return None if value == "" else value == "true"


class Runner:
    def __init__(self, binary: Path, events_bin: Path, jobs: int, root: Path, keep_runs: bool):
        self.binary, self.events_bin, self.jobs = binary, events_bin, max(1, jobs)
        self.root, self.keep_runs = root / "runs", keep_runs
        self.threads = max(1, (os.cpu_count() or 1) // self.jobs)
        self.root.mkdir(parents=True, exist_ok=True)
        self.invocations = 0
        self.baseline_bin: Path | None = None
        self.rust_calibration = False
        self._lock = threading.Lock()

    def command(self, kind: str, args: list) -> list[str]:
        if kind == "cli":
            return [str(self.binary), "cytoscan", *map(str, args)]
        if kind == "events":
            return [str(self.events_bin), *map(str, args)]
        if kind == "baseline_bin":
            return [str(self.baseline_bin), "cytoscan", *map(str, args)]
        raise ValueError(kind)

    def run(self, name: str, args: list, kind: str = "cli", features: bool = False,
            keep: bool = False) -> Run:
        output = self.root / name
        if output.exists():
            raise RuntimeError(f"run directory {output} already exists")
        command = self.command(kind, [*args, "--output", output])
        if kind != "events":
            command += ["--threads", str(self.threads)]
        started = time.monotonic()
        process = subprocess.run(command, capture_output=True, text=True,
                                 env={**os.environ, "RAYON_NUM_THREADS": str(self.threads)})
        wall = time.monotonic() - started
        with self._lock:
            self.invocations += 1
        if process.returncode != 0:
            if process.returncode in (1, 2) and any(marker in process.stderr for marker in FIT_FAILURES):
                if not keep:
                    shutil.rmtree(output, ignore_errors=True)
                return Run("fit_failed", reason=process.stderr.strip().splitlines()[-1], wall=wall)
            raise RuntimeError(f"{name}: exit {process.returncode}: {process.stderr.strip()}")
        run = parse_run(output, features)
        run.wall = wall
        if not (keep or self.keep_runs):
            shutil.rmtree(output)
        return run

    def map(self, specs: dict) -> dict:
        """Run `{key: (name, args, kind, features)}` with `jobs` concurrent invocations."""
        with concurrent.futures.ThreadPoolExecutor(self.jobs) as pool:
            futures = {key: pool.submit(self.run, *spec) for key, spec in specs.items()}
            return {key: future.result() for key, future in futures.items()}


def parse_run(output: Path, features: bool) -> Run:
    config = json.loads((output / "cytoscan_config.json").read_text())
    run = Run("ok", diagnostics=config["diagnostics"], parameters=config["parameters"])
    seeds_file = output / "cytoscan_stability_seeds.csv"
    if seeds_file.exists():
        rows = list(csv.DictReader(seeds_file.open()))
    else:
        threshold = config["diagnostics"].get("novelty_threshold")
        rows = [{**row, "seed_index": "0", "status": "ok",
                 "novelty_threshold": "" if threshold is None else repr(threshold)}
                for row in csv.DictReader((output / "cytoscan_summary.csv").open())]
    for row in rows:
        index = int(row["seed_index"])
        run.seed_status[index] = row["status"]
        entries = run.seeds.setdefault(index, {})
        if row["filename"] in entries:
            raise RuntimeError(f"{output}: duplicate file name {row['filename']}")
        entries[row["filename"]] = {
            "is_outlier": _bool(row["is_outlier"]), "outlier_score": _float(row["outlier_score"]),
            "is_novelty": _bool(row["is_novelty"]), "ll": _float(row["novelty_log_likelihood"]),
            "threshold": _float(row["novelty_threshold"]),
            "p_value": _float(row.get("novelty_p_value", "")),
            "ll_text": row["novelty_log_likelihood"]}
    if features:
        matrix = list(csv.DictReader((output / "cytoscan_features.csv").open()))
        run.features = {row["filename"]: [float(row[k]) for k in row if k not in ("filename", "path")]
                        for row in matrix}
    return run


# --------------------------------------------------------------------------- data
@dataclass
class Replicate:
    seed: int
    directory: Path
    references: list[Path]
    normals: list[Path]
    anomalies: list[Path]
    truth: dict
    shifted: dict = field(default_factory=dict)
    large: dict = field(default_factory=dict)


def stream(seed: int, *key: int) -> np.random.SeedSequence:
    return np.random.SeedSequence(seed, spawn_key=key)


def anomaly_specs(prefix: str = "anomaly") -> list[tuple[str, str]]:
    return [(f"{prefix}_{index:02d}_{kind}.fcs", kind)
            for index, kind in enumerate(kind for kind in ANOMALIES for _ in range(sc.ANOMALIES_PER_CLASS))]


def generate(seed: int, directory: Path, shifted: bool, large: bool) -> Replicate:
    """Every file of one replicate; file i of a role does not depend on the
    pool size or on other roles (one spawned generator per file)."""
    references = [(f"reference_{i:03d}.fcs", None) for i in range(sc.REFERENCE_POOL)]
    normals = [(f"normal_{i:03d}.fcs", None) for i in range(sc.NORMAL_TESTS)]
    anomalies = anomaly_specs()
    write_files(directory / "reference", stream(seed, sc.STREAMS["reference"]), references, sc.EVENTS)
    write_files(directory / "normal", stream(seed, sc.STREAMS["normal"]), normals, sc.EVENTS)
    write_files(directory / "anomaly", stream(seed, sc.STREAMS["anomaly"]), anomalies, sc.EVENTS)
    truth = {name: None for name, _ in references + normals} | dict(anomalies)
    replicate = Replicate(seed, directory, [directory / "reference" / n for n, _ in references],
                          [directory / "normal" / n for n, _ in normals],
                          [directory / "anomaly" / n for n, _ in anomalies], truth)
    if shifted:
        for level, effect in sc.SHIFT_LEVELS.items():
            specs = [(f"shift-{level}_{i:03d}.fcs", None) for i in range(sc.NORMAL_TESTS)]
            # Every level re-creates the same stream: paired latent draws.
            write_files(directory / f"shift-{level}", stream(seed, sc.STREAMS["shifted"]), specs,
                        sc.EVENTS, Batch(**effect))
            replicate.shifted[level] = [directory / f"shift-{level}" / n for n, _ in specs]
            replicate.truth |= {name: None for name, _ in specs}
    if large:
        pool = max(sc.FAMILIES["large_events"]["sizes"])
        roles = {"reference": [(f"large-reference_{i:03d}.fcs", None) for i in range(pool)],
                 "normal": [(f"large-normal_{i:03d}.fcs", None) for i in range(sc.NORMAL_TESTS)],
                 "anomaly": anomaly_specs("large-anomaly")}
        for role, specs in roles.items():
            write_files(directory / f"large-{role}", stream(seed, sc.STREAMS["events"], sc.STREAMS[role]),
                        specs, sc.EVENTS_LARGE)
            replicate.large[role] = [directory / f"large-{role}" / n for n, _ in specs]
            replicate.truth |= dict(specs)
    return replicate


def inputs_digest(directory: Path) -> str:
    digests = sorted(f"{p.relative_to(directory)}:{sha256(p)}" for p in directory.rglob("*.fcs"))
    return hashlib.sha256("\n".join(digests).encode()).hexdigest()


# --------------------------------------------------------------------------- arms
def run_args(tests, references, method, channels, flag, seeds=None, events=None):
    args = ["--channels", ",".join(channels), "--method", method, "--flag-method", flag]
    for path in tests:
        args += ["--input", path]
    for path in references:
        args += ["--reference", path]
    if seeds:
        args += ["--stability-seeds", seeds]
    if events is not None:
        args += ["--events-per-file", events]
    return args


def novelty_seeds(run: Run, count: int) -> list[int]:
    """Seed indices < count whose novelty values exist."""
    return [j for j in range(count) if run.seed_status.get(j) == "ok"]


def flip_metrics(flags_by_seed: list[dict], files: list[str], normals: set[str]) -> dict:
    """Agreement of per-seed decisions `flags_by_seed[j][file]` over the given seeds."""
    if len(flags_by_seed) < 2:
        return {"flip_all": None, "flip_normals": None, "unanimous": None, "kappa": None}
    def counts(names):
        return [(sum(bool(seed[name]) for seed in flags_by_seed), len(flags_by_seed)) for name in names]
    all_counts = counts(files)
    matrix = np.array([[seed[name] for seed in flags_by_seed] for name in files], dtype=bool)
    return {"flip_all": arms.pairwise_disagreement(all_counts),
            "flip_normals": arms.pairwise_disagreement(counts([f for f in files if f in normals])),
            "unanimous": float(np.mean([flagged in (0, total) for flagged, total in all_counts])),
            "kappa": arms.fleiss_kappa(matrix)}


def outcome(flags: dict, scores: dict, truth: dict, normals: list[str]) -> dict:
    """FPR, detection per anomaly class and AUROC of one replicate's decisions."""
    fp = sum(bool(flags[name]) for name in normals)
    detection = {}
    for kind in ANOMALIES:
        names = [name for name, label in truth.items() if label == kind and name in flags]
        detection[kind] = [sum(bool(flags[name]) for name in names), len(names)]
    anomalous = [scores[name] for name in flags if truth.get(name)]
    normal_scores = [scores[name] for name in normals]
    return {"fp": fp, "normals": len(normals), "fpr": fp / len(normals), "detection": detection,
            "auroc": arms.auroc(anomalous, normal_scores)}


def decided(record: dict, flags: dict, scores: dict, truth: dict, normals: list[str],
            stability: dict, margin_sd: dict | None = None) -> dict:
    record.update(status="decided", **outcome(flags, scores, truth, normals), **stability)
    if margin_sd is not None:
        record.update(margin_sd)
    return record


def margin_spread(run: Run, seeds: list[int], files: list[str]) -> dict:
    deviations, infinite = [], 0
    for name in files:
        margins = [arms.margin(run.seeds[j][name]["ll"], run.seeds[j][name]["threshold"]) for j in seeds]
        finite = [m for m in margins if math.isfinite(m)]
        infinite += len(margins) - len(finite)
        if len(finite) >= 2:
            deviations.append(statistics.stdev(finite))
    return {"margin_sd_median": float(np.median(deviations)) if deviations else None,
            "margin_sd_p90": float(np.quantile(deviations, 0.9)) if deviations else None,
            "margin_nonfinite": infinite}


def check_rust_disagreement(run: Run, files: list[str]) -> None:
    """Equivalence 4: production's `pairwise_disagreement` over every seed."""
    stability = run.diagnostics.get("stability")
    if not stability or not stability.get("novelty"):
        return
    seeds = novelty_seeds(run, len(stability["seeds"]))
    counts = [(sum(bool(run.seeds[j][name]["is_novelty"]) for j in seeds), len(seeds)) for name in files]
    expected = arms.pairwise_disagreement(counts)
    actual = stability["novelty"]["pairwise_disagreement"]
    if actual != expected:
        raise AssertionError(f"Rust pairwise_disagreement {actual} != {expected}")


def novelty_arm(run: Run, base: dict, files, truth, normals, count=sc.STABILITY_SEEDS) -> dict:
    """Decision at seed 0 and agreement over seeds 0..count-1 of one novelty run."""
    record = dict(base)
    if run.status != "ok":
        return record | {"status": "fit_failed", "reason": run.reason}
    seeds = novelty_seeds(run, count)
    flags = {name: run.seeds[0][name]["is_novelty"] for name in files}
    scores = {name: -run.seeds[0][name]["ll"] for name in files}
    stability = flip_metrics([{n: run.seeds[j][n]["is_novelty"] for n in files} for j in seeds], files, set(normals))
    stability["seeds_assessed"] = len(seeds)
    return decided(record, flags, scores, truth, normals, stability, margin_spread(run, seeds, files))


def ensemble_arm(run: Run, base: dict, files, truth, normals) -> dict:
    """`seed_ensemble_5`: mean margin over seeds 0-4 <= 0; its stability is the
    disagreement with the same rule over the independent seeds 5-9."""
    record = dict(base)
    if run.status != "ok":
        return record | {"status": "fit_failed", "reason": run.reason}
    groups = DECISION_RULE["ensemble_seed_groups"]
    available = set(novelty_seeds(run, sc.BASELINE_SEEDS))
    def decide_group(group):
        if not set(group) <= available:
            return None
        means = {n: float(np.mean([arms.margin(run.seeds[j][n]["ll"], run.seeds[j][n]["threshold"])
                                   for j in group])) for n in files}
        return means
    first, second = decide_group(groups[0]), decide_group(groups[1])
    if first is None:
        return record | {"status": "fit_failed", "reason": "a seed of the first ensemble failed"}
    flags = {n: first[n] <= 0 for n in files}
    if second is not None:
        stability = flip_metrics([flags, {n: second[n] <= 0 for n in files}], files, set(normals))
    else:
        stability = {"flip_all": None, "flip_normals": None, "unanimous": None, "kappa": None}
    stability["seeds_assessed"] = 2 if second is not None else 1
    return decided(record, flags, {n: -first[n] for n in files}, truth, normals, stability)


def conformal_arms(fold_runs: list[Run] | None, fold_members: list[list[str]], base: dict, files,
                   truth, normals, n: int, in_sample_threshold: float | None) -> tuple[dict, dict, dict]:
    """`cross_conformal`, `cross_conformal_ensemble_5` and the reference-only
    out-of-fold exceedance of the in-sample threshold."""
    plain, ensemble = dict(base), dict(base) | {"arm": "cross_conformal_ensemble_5"}
    if fold_runs is None:
        status = {"status": "abstained", "reason": f"{n} reference files < {sc.MIN_CONFORMAL_REFERENCE}"}
        return plain | status, ensemble | status, {"status": "not_assessed"}
    failed = [run for run in fold_runs if run.status != "ok"]
    if failed:
        status = {"status": "abstained", "reason": "fold fit failed: " + str(failed[0].reason)}
        return plain | status, ensemble | status, {"status": "fit_failed"}
    seeds = [j for j in range(sc.STABILITY_SEEDS) if all(run.seed_status.get(j) == "ok" for run in fold_runs)]
    per_seed, near_ties = {}, 0
    for j in seeds:
        counts = {}
        for name in files:
            scored = [([run.seeds[j][m]["ll"] for m in members], run.seeds[j][name]["ll"])
                      for run, members in zip(fold_runs, fold_members)]
            counts[name] = arms.conformal_count(scored)
            near_ties += sum(1 for held, test in scored for value in held if abs(value - test) < 1e-9)
        per_seed[j] = counts
    if 0 not in per_seed:
        raise AssertionError("fold runs succeeded but seed 0 is missing")
    flags = {name: arms.conformal_novel(per_seed[0][name], n) for name in files}
    scores = {name: -arms.conformal_p(per_seed[0][name], n) for name in files}
    by_seed = [{name: arms.conformal_novel(per_seed[j][name], n) for name in files} for j in seeds]
    stability = flip_metrics(by_seed, files, set(normals)) | {"seeds_assessed": len(seeds)}
    plain = decided(plain, flags, scores, truth, normals, stability)
    plain["near_ties"] = near_ties
    majority = {name: sum(seed[name] for seed in by_seed) * 2 > len(by_seed) for name in files}
    ensemble = decided(ensemble, majority, scores, truth, normals,
                       {"flip_all": None, "flip_normals": None, "unanimous": None, "kappa": None})
    exceed = sum(1 for run, members in zip(fold_runs, fold_members)
                 for m in members if run.seeds[0][m]["ll"] <= run.seeds[0][m]["threshold"])
    return plain, ensemble, {"status": "assessed", "out_of_fold_exceedance": exceed / n,
                             "in_sample_threshold": in_sample_threshold}


def fold_specs(prefix: str, references: list[Path], tests: list[Path], method: str, channels) -> tuple[dict, list]:
    n = len(references)
    specs, members = {}, []
    for k, held in enumerate(arms.folds(n, sc.FOLDS_MAX)):
        held_paths = [references[i] for i in held]
        train = [references[i] for i in range(n) if i not in set(held)]
        specs[f"{prefix}-fold{k}"] = (f"{prefix}-fold{k}", run_args(held_paths + tests, train, method, channels,
                                                                   "novelty", sc.STABILITY_SEEDS), "cli", False)
        members.append([p.name for p in held_paths])
    return specs, members


# --------------------------------------------------------------------------- families
def cell_name(method: str, channels: int, n: int, level: str | None = None) -> str:
    return f"{method}-c{channels}-n{n}" + (f"-{level}" if level else "")


def calibration_cells(runner: Runner, replicate: Replicate, family: str, methods, channel_counts, sizes,
                      tests: list[Path], references: list[Path], with_events: bool,
                      with_conformal: bool, records: list, extras: dict) -> dict:
    """Baseline (+ ensemble), event arms and cross-conformal for every cell of a family."""
    files = [p.name for p in tests]
    normals = [p.name for p in tests if replicate.truth[p.name] is None]
    specs, folds_of = {}, {}
    for method in methods:
        for channels in channel_counts:
            chosen = CHANNELS[:channels]
            for n in sizes:
                cell = cell_name(method, channels, n)
                prefix = f"{replicate.seed}-{family}-{cell}"
                refs = references[:n]
                specs[(cell, "baseline")] = (f"{prefix}-baseline", run_args(
                    tests, refs, method, chosen, "both" if family == "exchangeable" else "novelty",
                    sc.BASELINE_SEEDS), "cli", False)
                if with_events:
                    for arm, events in sc.EVENT_ARMS.items():
                        specs[(cell, arm)] = (f"{prefix}-{arm}", run_args(
                            tests, refs, method, chosen, "novelty", sc.STABILITY_SEEDS, events), "events", False)
                if with_conformal and n >= sc.MIN_CONFORMAL_REFERENCE:
                    fold_spec, members = fold_specs(prefix, refs, tests, method, chosen)
                    folds_of[cell] = (list(fold_spec), members)
                    specs |= {(cell, key): spec for key, spec in fold_spec.items()}
    results = runner.map(specs)
    cells = {}
    for (cell, key), run in results.items():
        cells.setdefault(cell, {})[key] = run
    for cell, runs in cells.items():
        n = int(cell.split("-n")[1].split("-")[0])
        base = {"family": family, "cell": cell, "seed": replicate.seed}
        baseline = runs["baseline"]
        if baseline.status == "ok":
            check_rust_disagreement(baseline, files)
        baseline_record = novelty_arm(baseline, base | {"arm": "baseline"}, files, replicate.truth, normals)
        records.append(baseline_record)
        records.append(ensemble_arm(baseline, base | {"arm": "seed_ensemble_5"}, files, replicate.truth, normals))
        for arm in EVENT_ARM_NAMES if with_events else ():
            records.append(novelty_arm(runs[arm], base | {"arm": arm}, files, replicate.truth, normals))
        if with_conformal:
            if cell in folds_of:
                keys, members = folds_of[cell]
                fold_runs = [runs[key] for key in keys]
            else:
                fold_runs, members = None, []
            threshold = baseline.diagnostics.get("novelty_threshold") if baseline.status == "ok" else None
            plain, ensemble, exceedance = conformal_arms(fold_runs, members, base | {"arm": "cross_conformal"},
                                                         files, replicate.truth, normals, n, threshold)
            records += [plain, ensemble]
            if exceedance["status"] == "assessed":
                extras.setdefault("exceedance", []).append(
                    base | exceedance | {"baseline_fpr": baseline_record.get("fpr")})
    return cells


RUST_MODE = "conformal-ledoit-wolf-v1"
NEAR_TIE = 1e-6
"""Benchmark features are read back at 12 decimals, so a Ledoit-Wolf score of
the benchmark and of production may differ by ~1e-10; a disagreement is
accepted only where a held-out score lies within this of the test score."""


def rust_calibration(runner: Runner, replicate: Replicate, base: dict, references, tests, method,
                     chosen, n, by_seed: dict, files, normals, records, extras) -> None:
    """Stage 3: production's `--novelty-mode conformal-ledoit-wolf-v1` must
    reproduce this benchmark's Ledoit-Wolf cross-conformal decision and count,
    seed by seed."""
    run = runner.run(f"{replicate.seed}-{base['family']}-rust-{base['cell']}",
                     [*run_args(tests, references[:n], method, chosen, "novelty", sc.STABILITY_SEEDS),
                      "--novelty-mode", RUST_MODE])
    check = extras.setdefault("rust_calibration", {"cells": 0, "comparisons": 0, "near_tie_mismatches": 0})
    check["cells"] += 1
    if run.status != "ok":
        raise AssertionError(f"{base}: production {RUST_MODE} failed: {run.reason}")
    for j, outcome in by_seed.items():
        python = outcome["cross_conformal"]
        for index, name in enumerate(files):
            rust = run.seeds[j][name]
            check["comparisons"] += 1
            if python["status"] != "decided":
                if rust["is_novelty"] is not None:
                    raise AssertionError(f"{base} seed {j} {name}: benchmark {python['status']}, production decided")
                continue
            count = round(rust["p_value"] * (n + 1)) - 1 if rust["p_value"] is not None else None
            if rust["is_novelty"] == python["flags"][index] and count == python["counts"][index]:
                continue
            if python["min_gaps"][index] < NEAR_TIE:
                check["near_tie_mismatches"] += 1
                continue
            raise AssertionError(f"{base} seed {j} {name}: production {rust['is_novelty']}/{count} "
                                 f"!= benchmark {python['flags'][index]}/{python['counts'][index]}")
    record = base | {"arm": "rust_" + RUST_MODE.replace("-", "_")}
    flags = {name: run.seeds[0][name]["is_novelty"] for name in files}
    if any(flag is None for flag in flags.values()):
        records.append(record | {"status": "abstained"})
        return
    seeds = [j for j in range(sc.STABILITY_SEEDS)
             if all(run.seeds[j][name]["is_novelty"] is not None for name in files)]
    stability = flip_metrics([{name: run.seeds[j][name]["is_novelty"] for name in files} for j in seeds],
                             files, set(normals)) | {"seeds_assessed": len(seeds)}
    records.append(decided(record, flags, {name: -run.seeds[0][name]["p_value"] for name in files},
                           replicate.truth, normals, stability))


def research_arms(runner: Runner, replicate: Replicate, family: str, methods, channel_counts, sizes,
                  tests, references, records, with_r: bool, extras: dict) -> None:
    """Covariance arms on production features of the whole pool, computed at
    seeds 0-4 (one feature run per seed) so their flags have a flip rate."""
    files = [p.name for p in tests]
    normals = [p.name for p in tests if replicate.truth[p.name] is None]
    for method in methods:
        for channels in channel_counts:
            chosen = CHANNELS[:channels]
            specs = {j: (f"{replicate.seed}-{family}-features-{method}-c{channels}-s{j}",
                         [*run_args([*references, *tests], [], method, chosen, "outlier"),
                          "--seed", arms.stability_seed(sc.PRIMARY_SEED, j)], "cli", True)
                     for j in range(sc.STABILITY_SEEDS)}
            runs = runner.map(specs)
            matrices = {j: (np.array([run.features[p.name] for p in references]),
                            np.array([run.features[name] for name in files])) for j, run in runs.items()}
            for n in sizes:
                base = {"family": family, "cell": cell_name(method, channels, n), "seed": replicate.seed}
                for arm, scorer in arms.RESEARCH_SCORERS.items():
                    by_seed = {j: arms.research_decisions(scorer, reference[:n], test, sc.FOLDS_MAX,
                                                          sc.MIN_CONFORMAL_REFERENCE)
                               for j, (reference, test) in matrices.items()}
                    for variant, suffix in (("in_sample", ""), ("cross_conformal", "+cross_conformal")):
                        entry = by_seed[0][variant]
                        record = base | {"arm": arm + suffix}
                        if entry["status"] != "decided":
                            records.append(record | {"status": entry["status"]})
                            continue
                        decided_seeds = [by_seed[j][variant] for j in sorted(by_seed)
                                         if by_seed[j][variant]["status"] == "decided"]
                        flags_by_seed = [dict(zip(files, e["flags"])) for e in decided_seeds]
                        stability = flip_metrics(flags_by_seed, files, set(normals))
                        stability["seeds_assessed"] = len(decided_seeds)
                        records.append(decided(record, dict(zip(files, entry["flags"])),
                                               dict(zip(files, entry["scores"])), replicate.truth, normals,
                                               stability))
                    if runner.rust_calibration and arm == "shrinkage_ledoit_wolf":
                        rust_calibration(runner, replicate, base, references, tests, method, chosen, n,
                                         by_seed, files, normals, records, extras)
                if with_r:
                    reference, test = matrices[0]
                    records.append(mclust_prior(runner, replicate, base, reference[:n], test, files, normals))


def mclust_prior(runner, replicate, base, reference, tests, files, normals) -> dict:
    """Research only: R `Mclust(prior = priorControl())`, in-sample 5% threshold."""
    directory = runner.root / f"{replicate.seed}-{base['family']}-prior-{base['cell']}"
    directory.mkdir()
    np.savetxt(directory / "reference.csv", reference, delimiter=",")
    np.savetxt(directory / "test.csv", tests, delimiter=",")
    process = subprocess.run(["Rscript", str(ROOT / "parity/scientific/novelty_prior.R"), str(directory)],
                             capture_output=True, text=True,
                             env={**os.environ, "R_LIBS_USER": str(ROOT / "parity/.env/Rlib")})
    record = base | {"arm": "mclust_prior"}
    if process.returncode != 0:
        shutil.rmtree(directory)
        return record | {"status": "fit_failed", "reason": process.stderr.strip()[-200:]}
    values = np.loadtxt(directory / "test_density.csv", delimiter=",", ndmin=1)
    threshold = float((directory / "threshold.txt").read_text())
    shutil.rmtree(directory)
    flags = {name: bool(v <= threshold) for name, v in zip(files, values)}
    scores = {name: -float(v) for name, v in zip(files, values)}
    return decided(record, flags, scores, replicate.truth, normals,
                   {"flip_all": None, "flip_normals": None, "unanimous": None, "kappa": None})


def outlier_within_cohort(runner: Runner, replicate: Replicate, extras: dict) -> None:
    """The outlier flag on 40 normals with and without the 10 anomalies, and on
    batch-shifted cohorts: it ranks files within their cohort."""
    normals = [p.name for p in replicate.normals]
    for method in ("summary", "quantiles"):
        mixed = runner.run(f"{replicate.seed}-outlier-mixed-{method}",
                           run_args(replicate.normals + replicate.anomalies, [], method, CHANNELS, "outlier",
                                    sc.STABILITY_SEEDS))
        alone = runner.run(f"{replicate.seed}-outlier-normals-{method}",
                           run_args(replicate.normals, [], method, CHANNELS, "outlier", sc.STABILITY_SEEDS))
        for label, run, cohort in (("normals_and_anomalies", mixed, replicate.normals + replicate.anomalies),
                                   ("normals_only", alone, replicate.normals)):
            names = [p.name for p in cohort]
            flags = [{n: run.seeds[j][n]["is_outlier"] for n in names} for j in range(sc.STABILITY_SEEDS)]
            entry = {"seed": replicate.seed, "method": method, "cohort": label,
                     "normal_flags": sum(bool(run.seeds[0][n]["is_outlier"]) for n in normals),
                     "normals": len(normals),
                     "anomalies_flagged": sum(bool(run.seeds[0][p.name]["is_outlier"])
                                              for p in cohort if replicate.truth[p.name]),
                     "anomalies": sum(1 for p in cohort if replicate.truth[p.name]),
                     **flip_metrics(flags, names, set(normals))}
            extras.setdefault("outlier", []).append(entry)


def batch_shift(runner: Runner, replicate: Replicate, records: list, extras: dict) -> None:
    family = sc.FAMILIES["batch_shift"]
    tests = [p for level in family["levels"] for p in replicate.shifted[level]]
    level_of = {p.name: level for level in family["levels"] for p in replicate.shifted[level]}
    specs, folds_of = {}, {}
    for method in family["methods"]:
        for n in family["sizes"]:
            cell = cell_name(method, 6, n)
            refs = replicate.references[:n]
            prefix = f"{replicate.seed}-batch-{cell}"
            specs[(cell, "baseline")] = (f"{prefix}-baseline", run_args(tests, refs, method, CHANNELS, "novelty",
                                                                        sc.STABILITY_SEEDS), "cli", False)
            if n >= sc.MIN_CONFORMAL_REFERENCE:
                fold_spec, members = fold_specs(prefix, refs, tests, method, CHANNELS)
                folds_of[cell] = (list(fold_spec), members)
                specs |= {(cell, key): spec for key, spec in fold_spec.items()}
    for level in family["levels"]:
        specs[("outlier", level)] = (f"{replicate.seed}-batch-outlier-{level}", run_args(
            replicate.shifted[level], [], "quantiles", CHANNELS, "outlier"), "cli", False)
    results = runner.map(specs)
    names = [p.name for p in tests]
    for method in family["methods"]:
        for n in family["sizes"]:
            cell = cell_name(method, 6, n)
            baseline = results[(cell, "baseline")]
            keys, members = folds_of.get(cell, (None, []))
            fold_runs = [results[(cell, key)] for key in keys] if keys else None
            arms_flags = {}
            if baseline.status == "ok":
                arms_flags["baseline"] = {name: baseline.seeds[0][name]["is_novelty"] for name in names}
            plain, _, _ = conformal_arms(fold_runs, members, {"arm": "cross_conformal"}, names,
                                         {name: None for name in names}, names, n, None)
            if plain.get("status") == "decided":
                count_flags = {}
                for j in (0,):
                    for name in names:
                        scored = [([run.seeds[j][m]["ll"] for m in mem], run.seeds[j][name]["ll"])
                                  for run, mem in zip(fold_runs, members)]
                        count_flags[name] = arms.conformal_novel(arms.conformal_count(scored), n)
                arms_flags["cross_conformal"] = count_flags
            for arm in ("baseline", "cross_conformal"):
                for level in family["levels"]:
                    level_names = [name for name in names if level_of[name] == level]
                    record = {"family": "batch_shift", "cell": cell_name(method, 6, n, level), "seed": replicate.seed,
                              "arm": arm, "level": level}
                    if arm not in arms_flags:
                        status = baseline.status if arm == "baseline" else plain.get("status")
                        records.append(record | {"status": "fit_failed" if status == "fit_failed" else "abstained"})
                        continue
                    fp = sum(bool(arms_flags[arm][name]) for name in level_names)
                    records.append(record | {"status": "decided", "fp": fp, "normals": len(level_names),
                                             "fpr": fp / len(level_names)})
    for level in family["levels"]:
        run = results[("outlier", level)]
        level_names = [p.name for p in replicate.shifted[level]]
        extras.setdefault("batch_outlier", []).append(
            {"seed": replicate.seed, "level": level,
             "flags": sum(bool(run.seeds[0][n]["is_outlier"]) for n in level_names), "files": len(level_names)})


def real_donors(runner: Runner) -> dict:
    """Leave-one-donor-out on the four published unstimulated donors."""
    config = sc.REAL_DONORS
    fixtures = ROOT / config["fixtures"]
    files = [fixtures / name for name in config["files"]]
    with (fixtures / "panel.csv").open() as handle:
        type_markers = [row["fcs_colname"] for row in csv.DictReader(handle) if row["marker_class"] == "type"]
    results = []
    for count in config["channel_counts"]:
        channels = type_markers if count == "all_type" else type_markers[:count]
        for method in config["methods"]:
            for held in files:
                others = [p for p in files if p != held]
                run = runner.run(f"real-{len(channels)}-{method}-{held.stem}",
                                 run_args([held], others, method, channels, "novelty", config["stability_seeds"]))
                entry = {"channels": len(channels), "method": method, "held_out": held.name,
                         "reference_donors": len(others), "status": run.status}
                if run.status == "ok":
                    values = {run.seeds[j][held.name]["ll_text"] for j in novelty_seeds(run, config["stability_seeds"])}
                    declared = run.diagnostics["novelty_flag"]["seed_dependence"]
                    identical = len(values) == 1
                    if declared == "seed_invariant" and not identical:
                        raise AssertionError(f"{held.name} {method}: declared seed_invariant but seeds differ")
                    entry |= {"flagged": run.seeds[0][held.name]["is_novelty"],
                              "calibration_status": run.diagnostics["novelty_flag"]["calibration_status"],
                              "seed_dependence": declared, "seeds_identical": identical,
                              "stability_status": run.diagnostics["novelty_flag"]["stability_status"],
                              "cross_conformal": "abstained (3 reference donors < 19)"}
                else:
                    entry["reason"] = run.reason
                results.append(entry)
    return {"normal_cohort_status": "insufficient_independent_normals",
            "population_fpr": {"status": "not_assessed",
                               "reason": "4 subjects from one study, one CyTOF instrument and panel; leave-one-out splits are not independent replications"},
            "seed_scope": "files have 438-881 events and the leave-one-out aggregate share is 3334, so no method subsamples: every seed is identical; only reference membership varies",
            "subjects": len(files), "results": results}


def contract_checks(runner: Runner, directory: Path) -> dict:
    """Panel differences are refused, not scored: a missing channel and, with
    --strict, a file without spillover metadata."""
    from fcsio import read_fcs, write_fcs

    write_files(directory / "reference", stream(sc.CI_SEEDS[0], 9), [(f"r{i}.fcs", None) for i in range(3)], 1000)
    write_files(directory / "test", stream(sc.CI_SEEDS[0], 10), [("t.fcs", None)], 1000)
    data, names = read_fcs(directory / "test" / "t.fcs")
    write_fcs(directory / "missing" / "t.fcs", data[:, :-1], names[:-1])
    write_fcs(directory / "nospill" / "t.fcs", data, names)
    references = sorted((directory / "reference").glob("*.fcs"))
    results = {}
    for label, test, extra in (("missing_channel", directory / "missing" / "t.fcs", []),
                               ("no_spillover_strict", directory / "nospill" / "t.fcs", ["--strict"])):
        output = runner.root / f"contract-{label}"
        process = subprocess.run(runner.command("cli", [*run_args([test], references, "summary", CHANNELS, "novelty"),
                                                        *extra, "--output", output]),
                                 capture_output=True, text=True)
        refused = process.returncode == 2 and not (output / "cytoscan_summary.csv").exists()
        if not refused:
            raise AssertionError(f"contract {label}: exit {process.returncode}, expected a refusal (2)")
        results[label] = {"exit_code": process.returncode, "refused": True,
                          "message": process.stderr.strip().splitlines()[-1]}
        shutil.rmtree(output, ignore_errors=True)
    return results


# --------------------------------------------------------------------------- equivalence
def equivalence(runner: Runner, replicate: Replicate, baseline_bin: Path | None) -> dict:
    """Checks that make the composition trustworthy; any mismatch raises."""
    tests = replicate.normals + replicate.anomalies
    refs = replicate.references[:20]
    columns = ("is_outlier", "outlier_score", "is_novelty", "novelty_log_likelihood")
    result = {"stability_rows_equal_seed_runs": [], "events_example_equals_cli": None,
              "baseline_binary": {"status": "not_assessed", "reason": "no --baseline-bin"}}
    for method in ("summary", "quantiles"):
        args = run_args(tests, refs, method, CHANNELS, "both")
        stable_dir = runner.root / f"equivalence-{method}-stability"
        subprocess.run(runner.command("cli", [*args, "--stability-seeds", 5, "--output", stable_dir]),
                       check=True, capture_output=True)
        seeds_rows = list(csv.DictReader((stable_dir / "cytoscan_stability_seeds.csv").open()))
        for j in range(1, 5):
            seed_dir = runner.root / f"equivalence-{method}-seed{j}"
            subprocess.run(runner.command("cli", [*args, "--seed", arms.stability_seed(sc.PRIMARY_SEED, j),
                                                  "--output", seed_dir]), check=True, capture_output=True)
            summary = {row["filename"]: row for row in csv.DictReader((seed_dir / "cytoscan_summary.csv").open())}
            rows = [row for row in seeds_rows if row["seed_index"] == str(j)]
            if len(rows) != len(summary):
                raise AssertionError("stability rows differ in count from the seed run")
            for row in rows:
                for column in columns:
                    if row[column] != summary[row["filename"]][column]:
                        raise AssertionError(f"{method} seed {j} {row['filename']} {column}: "
                                             f"{row[column]} != {summary[row['filename']][column]}")
            shutil.rmtree(seed_dir)
        result["stability_rows_equal_seed_runs"].append({"method": method, "seeds": [1, 2, 3, 4], "equal": True})
        plain_dir = runner.root / f"equivalence-{method}-plain"
        subprocess.run(runner.command("cli", [*args, "--output", plain_dir]), check=True, capture_output=True)
        for name in ("cytoscan_summary.csv", "cytoscan_features.csv"):
            if (plain_dir / name).read_bytes() != (stable_dir / name).read_bytes():
                raise AssertionError(f"--stability-seeds changed {name}")
        if method == "quantiles":
            events_dir = runner.root / "equivalence-events-1000"
            subprocess.run(runner.command("events", [*args, "--events-per-file", 1000, "--output", events_dir]),
                           check=True, capture_output=True)
            for name in ("cytoscan_summary.csv", "cytoscan_features.csv"):
                if (plain_dir / name).read_bytes() != (events_dir / name).read_bytes():
                    raise AssertionError(f"example at 1000 events differs from the CLI in {name}")
            result["events_example_equals_cli"] = True
            shutil.rmtree(events_dir)
        if baseline_bin is not None:
            result["baseline_binary"] = baseline_check(runner, args, plain_dir, stable_dir, method,
                                                       result["baseline_binary"])
        shutil.rmtree(stable_dir)
        shutil.rmtree(plain_dir)
    return result


def baseline_check(runner, args, plain_dir, stable_dir, method, previous) -> dict:
    """The candidate's default outputs equal the clean-main binary's, byte for
    byte; the same holds for the primary outputs of a stability run."""
    main_dir = runner.root / f"equivalence-{method}-main"
    subprocess.run(runner.command("baseline_bin", [*args, "--output", main_dir]), check=True, capture_output=True)
    for name in ("cytoscan_summary.csv", "cytoscan_features.csv"):
        for candidate in (plain_dir, stable_dir):
            if (main_dir / name).read_bytes() != (candidate / name).read_bytes():
                raise AssertionError(f"{candidate.name}/{name} differs from the baseline binary")
    main = json.loads((main_dir / "cytoscan_config.json").read_text())
    new = json.loads((plain_dir / "cytoscan_config.json").read_text())
    for document in (main, new):
        document["parameters"].pop("output_dir")
    if main["parameters"] != new["parameters"]:
        raise AssertionError("parameters differ from the baseline binary")
    for key, value in main["diagnostics"].items():
        if new["diagnostics"].get(key) != value:
            raise AssertionError(f"diagnostics.{key} differs from the baseline binary")
    added = sorted(set(new["diagnostics"]) - set(main["diagnostics"]))
    timings = {"main": [], "candidate": []}
    for _ in range(3):
        for label, kind in (("main", "baseline_bin"), ("candidate", "cli")):
            directory = runner.root / f"equivalence-timing-{label}"
            started = time.monotonic()
            subprocess.run(runner.command(kind, [*args, "--output", directory]), check=True, capture_output=True)
            timings[label].append(time.monotonic() - started)
            shutil.rmtree(directory)
    shutil.rmtree(main_dir)
    ratio = statistics.median(timings["candidate"]) / statistics.median(timings["main"])
    methods = previous.get("methods", []) if previous.get("status") == "equal" else []
    return {"status": "equal", "binary_sha256": sha256(runner.baseline_bin),
            "methods": methods + [{"method": method, "added_diagnostics": added,
                                   "median_seconds": {k: statistics.median(v) for k, v in timings.items()},
                                   "time_ratio": ratio, "time_ratio_flag": ratio > 1.10}]}


# --------------------------------------------------------------------------- statistics
def summarize(records: list[dict]) -> dict:
    """Per (family, cell, arm) statistics over replicates."""
    rule = DECISION_RULE["bootstrap"]
    groups = {}
    for record in records:
        groups.setdefault((record["family"], record["cell"], record["arm"]), []).append(record)
    baseline = {(r["family"], r["cell"], r["seed"]): r for r in records if r["arm"] == "baseline"}
    summary = {}
    for (family, cell, arm), group in sorted(groups.items()):
        done = [r for r in group if r["status"] == "decided"]
        entry = {"replicates": len(group), "decided": len(done),
                 "abstained": sum(r["status"] == "abstained" for r in group),
                 "fit_failed": sum(r["status"] == "fit_failed" for r in group),
                 "decision_rate": len(done) / len(group)}
        if done:
            fpr = [r["fpr"] for r in done]
            fp, total = sum(r["fp"] for r in done), sum(r["normals"] for r in done)
            entry |= {"fpr_mean": float(np.mean(fpr)),
                      "fpr_ci95": arms.bootstrap_mean_interval(fpr, rule["replicates"], rule["seed"]),
                      "fpr_pooled_wilson95": wilson_interval(fp, total),
                      "fpr_pooled_note": "ignores within-replicate correlation",
                      "fpr_p50": float(np.quantile(fpr, 0.5)), "fpr_p90": float(np.quantile(fpr, 0.9)),
                      "fpr_max": float(max(fpr)),
                      "p_fpr_gt_2alpha": float(np.mean([v > 2 * sc.ALPHA[0] / sc.ALPHA[1] for v in fpr]))}
            if "detection" in done[0]:
                detection = {}
                for kind in ANOMALIES:
                    hits = sum(r["detection"][kind][0] for r in done)
                    count = sum(r["detection"][kind][1] for r in done)
                    detection[kind] = {"detected": hits, "total": count, "rate": hits / count if count else None,
                                       "wilson95": wilson_interval(hits, count) if count else None}
                entry["detection"] = detection
                aurocs = [r["auroc"] for r in done if r.get("auroc") is not None]
                entry["auroc_mean"] = float(np.mean(aurocs)) if aurocs else None
            for key in ("flip_all", "flip_normals", "unanimous", "kappa", "margin_sd_median", "margin_sd_p90"):
                values = [r[key] for r in done if r.get(key) is not None]
                entry[f"{key}_mean"] = float(np.mean(values)) if values else None
            if "margin_nonfinite" in done[0]:
                entry["margin_nonfinite"] = int(sum(r["margin_nonfinite"] for r in done))
            if "near_ties" in done[0]:
                entry["near_ties"] = int(sum(r["near_ties"] for r in done))
            if arm != "baseline":
                pairs = [(r, baseline.get((family, cell, r["seed"]))) for r in done]
                pairs = [(r, b) for r, b in pairs if b and b["status"] == "decided"]
                if pairs:
                    entry["paired_fpr_difference_ci95"] = arms.paired_difference_interval(
                        [r["fpr"] for r, _ in pairs], [b["fpr"] for _, b in pairs], rule["replicates"], rule["seed"])
                    entry["paired_replicates"] = len(pairs)
        summary.setdefault(family, {}).setdefault(cell, {})[arm] = entry
    return summary


def cell_size(cell: str) -> int:
    return int(cell.split("-n")[1].split("-")[0])


def cell_method(cell: str) -> str:
    return cell.split("-c")[0]


def criteria(summary: dict, claim: str, arm: str, family: str, cell: str) -> dict:
    """Each criterion of `claim` for `arm` on one cell: True when met."""
    rule = DECISION_RULE["claims"][claim]
    entries = summary.get(family, {}).get(cell, {})
    entry, base = entries.get(arm), entries.get(DECISION_RULE["baseline_arm"])
    n = cell_size(cell)
    if entry is None or base is None:
        return {"evaluated": False}
    if claim == "calibration":
        if n < rule["must_abstain_below_reference"]:
            return {"evaluated": True, "abstains_below_reference": entry["decided"] == 0}
        if n < rule["reference_min"]:
            return {"evaluated": False}
        if entry["decided"] == 0:
            return {"evaluated": True, "decision_rate": False}
        result = {"evaluated": True,
                  "decision_rate": entry["decision_rate"] >= rule["decision_rate_min"],
                  "fpr_mean": entry["fpr_mean"] <= rule["fpr_mean_max"],
                  "fpr_upper95": entry["fpr_ci95"][1] <= rule["fpr_upper95_max"]}
        result["novelty_flip_rate"] = (entry.get("flip_normals_mean") is not None
                                       and entry["flip_normals_mean"] <= rule["novelty_flip_rate_max"])
        detection = rule["detection"]
        if family in detection["families"] and n >= detection["reference_min"]:
            result["detection"] = all((entry["detection"][kind]["rate"] or 0.0) >= detection["rate_min"]
                                      for kind in detection["classes"])
        return result
    # stability claim
    if base.get("flip_normals_mean") is None or base["flip_normals_mean"] < rule["baseline_flip_rate_min_to_evaluate"]:
        return {"evaluated": False, "reason": "baseline flip rate below the evaluation floor"}
    if entry["decided"] == 0:
        return {"evaluated": True, "decision_rate": False}
    result = {"evaluated": True,
              "flip_rate": entry.get("flip_normals_mean") is not None
              and entry["flip_normals_mean"] <= rule["flip_rate_ratio_max"] * base["flip_normals_mean"],
              "fpr_paired_difference": entry.get("paired_fpr_difference_ci95") is not None
              and entry["paired_fpr_difference_ci95"][1] <= rule["fpr_paired_difference_upper95_max"],
              "decision_rate": entry["decision_rate"] >= base["decision_rate"] - rule["decision_rate_margin_vs_baseline"]}
    if n >= rule["detection_reference_min"] and "detection" in entry:
        result["detection"] = all((entry["detection"][k]["rate"] or 0.0)
                                  >= (base["detection"][k]["rate"] or 0.0) - rule["detection_margin_vs_baseline"]
                                  for k in ANOMALIES)
    return result


def decide(summary: dict, profile: str, equivalence_passed: bool) -> dict:
    """Eligibility of every decision-eligible arm; never automatic adoption."""
    decision = {}
    for claim, rule in DECISION_RULE["claims"].items():
        for arm in rule["arms"]:
            failed, evaluated = [], 0
            for family in rule["families"]:
                for cell in sorted(summary.get(family, {})):
                    if cell_method(cell) not in DECISION_RULE["decision_methods"]:
                        continue
                    checks = criteria(summary, claim, arm, family, cell)
                    if not checks.get("evaluated"):
                        continue
                    evaluated += 1
                    failed += [f"{family}/{cell}: {name}" for name, ok in checks.items()
                               if name not in ("evaluated", "reason") and not ok]
            if evaluated == 0:
                failed.append("no cell could be evaluated" + (": nothing to stabilize" if claim == "stability" else ""))
            if not equivalence_passed:
                failed.append("equivalence checks did not pass")
            if profile != DECISION_RULE["decision_profile"]:
                failed.append(f"profile {profile} is not the decision profile")
            decision[arm] = {"claim": claim, "eligible": not failed, "cells_evaluated": evaluated,
                             "failed_criteria": failed,
                             "reason": "all predeclared criteria met" if not failed else f"{len(failed)} criteria not met"}
    return decision


def exceedance_validity(entries: list[dict]) -> dict:
    """Does the reference-only out-of-fold exceedance track the held-out FPR?"""
    from scipy.stats import spearmanr

    pairs = [(e["out_of_fold_exceedance"], e["baseline_fpr"]) for e in entries if e.get("baseline_fpr") is not None]
    if len(pairs) < 3:
        return {"status": "not_assessed", "pairs": len(pairs)}
    estimate, truth = np.array(pairs).T
    rho = spearmanr(estimate, truth).statistic
    return {"status": "assessed", "pairs": len(pairs), "mean_bias": float(np.mean(estimate - truth)),
            "spearman": None if not np.isfinite(rho) else float(rho),
            "mean_estimate": float(np.mean(estimate)), "mean_heldout_fpr": float(np.mean(truth))}


def outlier_summary(entries: list[dict]) -> dict:
    summary = {}
    for entry in entries:
        key = f"{entry['method']}/{entry['cohort']}"
        summary.setdefault(key, []).append(entry)
    result = {"scope": "within_test_cohort", "calibration_status": "uncalibrated_heuristic", "cohorts": {}}
    for key, group in summary.items():
        fp, total = sum(e["normal_flags"] for e in group), sum(e["normals"] for e in group)
        anomalies = sum(e["anomalies"] for e in group)
        result["cohorts"][key] = {
            "replicates": len(group), "normal_flag_rate": fp / total, "wilson95": wilson_interval(fp, total),
            "anomaly_detection": (sum(e["anomalies_flagged"] for e in group) / anomalies) if anomalies else None,
            "flip_normals_mean": float(np.mean([e["flip_normals"] for e in group if e["flip_normals"] is not None]))}
    return result


# --------------------------------------------------------------------------- driver
def profile_cells(family: str, profile: str):
    config = sc.FAMILIES[family]
    sizes = config.get("ci_sizes", config["sizes"]) if profile == "ci" else config["sizes"]
    return config["methods"], config["channels"], sizes


def run_benchmark(args) -> dict:
    profile = args.profile
    if profile == "confirmation":
        dirty = subprocess.run(["git", "status", "--porcelain", "--", "parity/scientific/novelty_*",
                                "parity/gen/cytoscan.py"], cwd=ROOT, capture_output=True, text=True).stdout
        if dirty.strip():
            raise SystemExit(f"confirmation requires a clean tree for the benchmark sources:\n{dirty}")
    runner = Runner(args.binary.resolve(), args.events_bin.resolve(), args.jobs, args.output, args.keep_runs)
    runner.baseline_bin = args.baseline_bin.resolve() if args.baseline_bin else None
    runner.rust_calibration = args.rust_calibration
    seeds = sc.PROFILE_SEEDS[profile]
    counts = {family: config["replicates"][profile] for family, config in sc.FAMILIES.items()}
    records, extras, digests, timing = [], {}, {}, {}
    report_progress = args.output / "progress.json"
    for index, seed in enumerate(seeds[:max(counts.values())]):
        started = time.monotonic()
        directory = args.output / "inputs" / str(seed)
        needs = {family: index < count for family, count in counts.items()}
        replicate = generate(seed, directory, needs["batch_shift"], needs["large_events"])
        digests[seed] = inputs_digest(directory)
        if index == 0:
            extras["equivalence"] = equivalence(runner, replicate, runner.baseline_bin)
        tests = replicate.normals + replicate.anomalies
        if needs["exchangeable"]:
            methods, channels, sizes = profile_cells("exchangeable", profile)
            calibration_cells(runner, replicate, "exchangeable", methods, channels, sizes, tests,
                              replicate.references, True, True, records, extras)
            research_arms(runner, replicate, "exchangeable", methods, channels, sizes, tests,
                          replicate.references, records, args.with_r_prior and profile == "development", extras)
            outlier_within_cohort(runner, replicate, extras)
        if needs["dimension"]:
            methods, channels, sizes = profile_cells("dimension", profile)
            calibration_cells(runner, replicate, "dimension", methods, channels, sizes, tests,
                              replicate.references, False, True, records, extras)
            research_arms(runner, replicate, "dimension", methods, channels, sizes, tests,
                          replicate.references, records, False, extras)
        if needs["aggregate_methods"]:
            methods, channels, sizes = profile_cells("aggregate_methods", profile)
            calibration_cells(runner, replicate, "aggregate_methods", methods, channels, sizes, tests,
                              replicate.references, False, True, records, extras)
        if needs["batch_shift"]:
            batch_shift(runner, replicate, records, extras)
        if needs["large_events"]:
            methods, channels, sizes = profile_cells("large_events", profile)
            large_tests = replicate.large["normal"] + replicate.large["anomaly"]
            calibration_cells(runner, replicate, "large_events", methods, channels, sizes, large_tests,
                              replicate.large["reference"], True, False, records, extras)
        if not args.keep_inputs:
            shutil.rmtree(directory)
        timing[seed] = time.monotonic() - started
        report_progress.write_text(json.dumps({"completed_replicates": index + 1, "last_seed": seed,
                                               "seconds": timing[seed], "invocations": runner.invocations},
                                              indent=2) + "\n")
        with (args.output / "replicates.jsonl").open("a") as journal:
            for record in records:
                if record["seed"] == seed:
                    journal.write(json.dumps(record, allow_nan=False) + "\n")
    summary = summarize(records)
    equivalence_passed = all(entry["equal"] for entry in extras["equivalence"]["stability_rows_equal_seed_runs"]) \
        and extras["equivalence"]["events_example_equals_cli"] is True
    write_replicates_csv(args.output / "replicates.csv", records)
    return {
        "families": summary,
        "decision": decide(summary, profile, equivalence_passed),
        "split_conformal_reference": [arms.split_conformal_reference(n, sc.NORMAL_TESTS)
                                      for n in sorted({s for f in sc.FAMILIES.values() for s in f["sizes"]})],
        "reference_only_exceedance": exceedance_validity(extras.get("exceedance", [])),
        "outlier_within_cohort": outlier_summary(extras.get("outlier", [])),
        "batch_outlier": extras.get("batch_outlier", []),
        "real_donors": real_donors(runner),
        "contract": contract_checks(runner, args.output / "contract"),
        "equivalence": extras["equivalence"],
        "rust_calibration": extras.get("rust_calibration", {"status": "not_assessed",
                                                            "reason": "run without --rust-calibration"}),
        "inputs_sha256": digests,
        "replicate_seconds": timing,
        "invocations": runner.invocations,
    }


def write_replicates_csv(path: Path, records: list[dict]) -> None:
    columns = ["family", "cell", "seed", "arm", "status", "fp", "normals", "fpr", "auroc", "flip_all",
               "flip_normals", "unanimous", "kappa", "margin_sd_median", "level", "reason"]
    with path.open("w", newline="") as handle:
        writer = csv.DictWriter(handle, columns, extrasaction="ignore")
        writer.writeheader()
        for record in records:
            writer.writerow(record)


def library_versions() -> dict:
    import pandas
    import scipy
    import sklearn
    return {"python": platform.python_version(), "numpy": np.__version__, "scipy": scipy.__version__,
            "sklearn": sklearn.__version__, "pandas": pandas.__version__}


def run_command(args) -> None:
    args.output.mkdir(parents=True, exist_ok=False)
    head = subprocess.run(["git", "rev-parse", "HEAD"], cwd=ROOT, capture_output=True, text=True).stdout.strip()
    report = {"schema_version": SCHEMA_VERSION, "benchmark": BENCHMARK, "profile": args.profile,
              "status": "failed", "decision_rule": DECISION_RULE, "decision_rule_sha256": DECISION_RULE_SHA256,
              "validation": "synthetic independent normal cohorts; real donors reported, population FPR not_assessed",
              "git_head": head, "binary_sha256": sha256(args.binary), "events_bin_sha256": sha256(args.events_bin),
              "scripts_sha256": {s: sha256(ROOT / s) for s in SCRIPTS if (ROOT / s).exists()},
              "libraries": library_versions(), "jobs": args.jobs,
              "arguments": {k: str(v) for k, v in vars(args).items() if k != "func"}}
    started = time.monotonic()
    try:
        report |= run_benchmark(args)
        report["status"] = "completed"
    except BaseException as error:
        report["error"] = f"{type(error).__name__}: {error}"
        raise
    finally:
        report["wall_seconds"] = time.monotonic() - started
        (args.output / "report.json").write_text(json.dumps(report, indent=2, allow_nan=False, default=str) + "\n")


# --------------------------------------------------------------------------- table
def fmt(value, digits=3) -> str:
    if value is None:
        return "–"
    if isinstance(value, list):
        return "[" + ", ".join(fmt(v, digits) for v in value) + "]"
    return f"{value:.{digits}f}"


def table_command(args) -> None:
    rows = []
    reports = [json.loads(path.read_text()) for path in args.report]
    for report in reports:
        if report.get("decision_rule_sha256") != DECISION_RULE_SHA256:
            raise SystemExit(f"report rule {report.get('decision_rule_sha256')} != {DECISION_RULE_SHA256}: "
                             "produced under another decision rule")
        if report.get("status") != "completed":
            raise SystemExit("report did not complete")
    args.output.mkdir(parents=True, exist_ok=True)
    lines = []
    for report in reports:
        lines += [f"# CytoScan novelty benchmark ({report['profile']})", "",
                  f"Rule `{report['decision_rule_sha256'][:12]}`, binary `{report['binary_sha256'][:12]}`, "
                  f"git `{report['git_head'][:12]}`, {report['wall_seconds'] / 60:.1f} min.", "",
                  "| family | cell | arm | decided | FPR mean [95% CI] | P(FPR>0.1) | flip (normals) | κ | AUROC | min detection |",
                  "|---|---|---|---|---|---|---|---|---|---|"]
        for family, cells in report["families"].items():
            for cell, by_arm in cells.items():
                for arm, entry in by_arm.items():
                    detection = entry.get("detection")
                    lowest = min((d["rate"] for d in detection.values() if d["rate"] is not None), default=None) if detection else None
                    lines.append(f"| {family} | {cell} | {arm} | {entry['decided']}/{entry['replicates']} | "
                                 f"{fmt(entry.get('fpr_mean'))} {fmt(entry.get('fpr_ci95'))} | "
                                 f"{fmt(entry.get('p_fpr_gt_2alpha'), 2)} | {fmt(entry.get('flip_normals_mean'))} | "
                                 f"{fmt(entry.get('kappa_mean'), 2)} | {fmt(entry.get('auroc_mean'))} | {fmt(lowest, 2)} |")
                    rows.append({"profile": report["profile"], "family": family, "cell": cell, "arm": arm,
                                 **{k: v for k, v in entry.items() if not isinstance(v, (dict, list))},
                                 "fpr_ci95_low": (entry.get("fpr_ci95") or [None, None])[0],
                                 "fpr_ci95_high": (entry.get("fpr_ci95") or [None, None])[1]})
        lines += ["", "## Decision", ""]
        for arm, verdict in report["decision"].items():
            lines.append(f"- `{arm}` ({verdict['claim']}): {'eligible' if verdict['eligible'] else 'not eligible'}"
                         f" — {verdict['reason']}")
            lines += [f"  - {item}" for item in verdict["failed_criteria"][:12]]
            if len(verdict["failed_criteria"]) > 12:
                lines.append(f"  - … {len(verdict['failed_criteria']) - 12} more")
        lines.append("")
    (args.output / "table.md").write_text("\n".join(lines) + "\n")
    if rows:
        columns = sorted({key for row in rows for key in row}, key=lambda k: (k not in ("profile", "family", "cell", "arm"), k))
        with (args.output / "table.csv").open("w", newline="") as handle:
            writer = csv.DictWriter(handle, columns)
            writer.writeheader()
            writer.writerows(rows)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    run = commands.add_parser("run")
    run.add_argument("--profile", choices=sc.PROFILES, required=True)
    run.add_argument("--binary", type=Path, required=True)
    run.add_argument("--events-bin", type=Path, required=True)
    run.add_argument("--baseline-bin", type=Path)
    run.add_argument("--jobs", type=int, default=4)
    run.add_argument("--with-r-prior", action="store_true")
    run.add_argument("--rust-calibration", action="store_true",
                     help=f"check production's --novelty-mode {RUST_MODE} against every Ledoit-Wolf cell")
    run.add_argument("--keep-inputs", action="store_true")
    run.add_argument("--keep-runs", action="store_true")
    run.add_argument("--output", type=Path, required=True)
    run.set_defaults(func=run_command)
    table = commands.add_parser("table")
    table.add_argument("--report", type=Path, action="append", required=True)
    table.add_argument("--output", type=Path, required=True)
    table.set_defaults(func=table_command)
    args = parser.parse_args()
    args.func(args)


if __name__ == "__main__":
    main()
