"""Pure statistics of the CytoScan novelty benchmark: no I/O, no CLI.

Decisions are made with integers wherever production makes them with
integers, so the Rust implementation of an eligible arm can be compared
with this composition exactly.
"""
from __future__ import annotations

import math

import numpy as np

from novelty_scenarios import ALPHA, STABILITY_SEED_STRIDE

U64 = 1 << 64


def stability_seed(primary: int, index: int) -> int:
    """`cytiq_cytoscan::stability_seed`: `primary + index * 2^32` (wrapping)."""
    if not 0 <= index < 64:
        raise ValueError(f"stability seed index {index}")
    return (primary + index * STABILITY_SEED_STRIDE) % U64


def folds(n: int, folds_max: int) -> list[list[int]]:
    """Reference indices (in resolved order) of each fold: `i mod K`, `K = min(n, folds_max)`."""
    k = min(n, folds_max)
    if k < 2:
        raise ValueError(f"{n} reference files cannot be split into folds")
    return [[i for i in range(n) if i % k == fold] for fold in range(k)]


def margin(log_likelihood: float, threshold: float) -> float:
    """`ll - threshold`; equal values (two equal infinities included) give 0."""
    if log_likelihood == threshold:
        return 0.0
    return log_likelihood - threshold


def conformal_count(fold_scores: list[tuple[list[float], float]]) -> int:
    """Cross-conformal count of one test file: `sum_k #{i in F_k : ll_k(i) <= ll_k(t)}`.

    `fold_scores[k]` holds the held-out reference log-likelihoods of fold k and
    the test file's log-likelihood, both under the model fitted without fold k.
    """
    return sum(sum(1 for value in held_out if value <= test) for held_out, test in fold_scores)


def conformal_novel(count: int, n: int) -> bool:
    """`p = (1 + c) / (n + 1) <= alpha` without floating point: `den (1 + c) <= num (n + 1)`."""
    numerator, denominator = ALPHA
    return denominator * (1 + count) <= numerator * (n + 1)


def conformal_p(count: int, n: int) -> float:
    return (1 + count) / (n + 1)


def quantile_type7(values, probability: float) -> float:
    """R's default quantile, as `novelty.rs::quantile_type7`."""
    ordered = sorted(values)
    position = probability * (len(ordered) - 1)
    lower, upper = math.floor(position), math.ceil(position)
    return ordered[lower] + (ordered[upper] - ordered[lower]) * (position - lower)


def pairwise_disagreement(counts: list[tuple[int, int]]) -> float | None:
    """As `stability.rs`: disagreeing seed pairs over all pairs, `(flagged, assessed)` per file."""
    pairs = sum(assessed * (assessed - 1) // 2 for _, assessed in counts)
    if pairs == 0:
        return None
    return sum(flagged * (assessed - flagged) for flagged, assessed in counts) / pairs


def fleiss_kappa(flags: np.ndarray) -> float | None:
    """Fleiss' kappa of a files x seeds boolean matrix (two categories).

    `None` when undefined: fewer than two seeds or files, or every decision is
    the same (expected agreement 1).
    """
    flags = np.asarray(flags, dtype=bool)
    if flags.ndim != 2 or flags.shape[0] < 2 or flags.shape[1] < 2:
        return None
    files, raters = flags.shape
    positive = flags.sum(axis=1)
    counts = np.column_stack([positive, raters - positive]).astype(float)
    agreement = ((counts * (counts - 1)).sum(axis=1) / (raters * (raters - 1))).mean()
    shares = counts.sum(axis=0) / (files * raters)
    expected = float((shares ** 2).sum())
    if expected >= 1.0:
        return None
    return float((agreement - expected) / (1.0 - expected))


def bootstrap_mean_interval(values, replicates: int, seed: int, level: float = 0.95) -> list[float] | None:
    """Percentile bootstrap interval of the mean, resampling the independent units."""
    values = np.asarray(values, dtype=float)
    if values.size == 0:
        return None
    rng = np.random.default_rng(seed)
    draws = rng.integers(0, values.size, (replicates, values.size))
    means = values[draws].mean(axis=1)
    tail = (1.0 - level) / 2.0
    return [float(np.quantile(means, tail)), float(np.quantile(means, 1.0 - tail))]


def paired_difference_interval(candidate, baseline, replicates: int, seed: int,
                               level: float = 0.95) -> list[float] | None:
    """Bootstrap interval of `mean(candidate - baseline)` over paired replicates."""
    candidate, baseline = np.asarray(candidate, float), np.asarray(baseline, float)
    if candidate.shape != baseline.shape:
        raise ValueError("paired samples differ in length")
    return bootstrap_mean_interval(candidate - baseline, replicates, seed, level)


def auroc(anomalous, normal) -> float | None:
    """Mann-Whitney AUROC with ties counted 0.5; higher score = more anomalous."""
    anomalous, normal = np.asarray(anomalous, float), np.asarray(normal, float)
    if anomalous.size == 0 or normal.size == 0:
        return None
    greater = (anomalous[:, None] > normal[None, :]).sum()
    ties = (anomalous[:, None] == normal[None, :]).sum()
    return float((greater + 0.5 * ties) / (anomalous.size * normal.size))


def split_conformal_reference(n: int, tests: int, alpha: tuple[int, int] = ALPHA,
                              limit: float = 0.10) -> dict:
    """What a perfectly exchangeable split-conformal rule gives with `n`
    calibration files and `tests` normal test files: the flagged fraction is
    `Beta(k, n + 1 - k)` with `k = floor((n + 1) alpha)`, so the false
    positives are beta-binomial. `None` values when the rule cannot flag."""
    from scipy.stats import betabinom

    k = (n + 1) * alpha[0] // alpha[1]
    if k < 1:
        return {"n_reference": n, "k": 0, "mean_fpr": 0.0, "p_fpr_gt_limit": 0.0,
                "abstains": True}
    distribution = betabinom(tests, k, n + 1 - k)
    above = math.floor(limit * tests)
    return {"n_reference": n, "k": k, "mean_fpr": k / (n + 1),
            "p_fpr_gt_limit": float(distribution.sf(above)), "abstains": False}


def gaussian_log_density(train: np.ndarray, rows: np.ndarray, covariance: np.ndarray) -> np.ndarray:
    """Log-density of `rows` under N(mean(train), covariance)."""
    centre = train.mean(axis=0)
    sign, logdet = np.linalg.slogdet(covariance)
    if sign <= 0:
        raise np.linalg.LinAlgError("covariance is not positive definite")
    solved = np.linalg.solve(covariance, (rows - centre).T).T
    quadratic = np.einsum("ij,ij->i", rows - centre, solved)
    return -0.5 * (quadratic + logdet + train.shape[1] * math.log(2 * math.pi))


def ledoit_wolf_scores(train: np.ndarray, rows: np.ndarray) -> np.ndarray:
    """Research only: one Gaussian with Ledoit-Wolf shrunk covariance."""
    from sklearn.covariance import LedoitWolf

    covariance = LedoitWolf().fit(train).covariance_
    return gaussian_log_density(train, rows, covariance)


def pca_scores(train: np.ndarray, rows: np.ndarray) -> np.ndarray:
    """Research only: a diagonal Gaussian on the reference's leading principal
    components, `k = min(rank, n // 5)` (at least 1)."""
    centre = train.mean(axis=0)
    _, singular, axes = np.linalg.svd(train - centre, full_matrices=False)
    rank = int(np.sum(singular > np.finfo(float).eps * max(train.shape) * singular[0]))
    k = max(1, min(rank, len(train) // 5))
    projection = axes[:k].T
    reduced, reduced_rows = (train - centre) @ projection, (rows - centre) @ projection
    variance = np.maximum(reduced.var(axis=0, ddof=1), 1e-12)
    return -0.5 * (np.sum(reduced_rows ** 2 / variance, axis=1)
                   + np.sum(np.log(variance)) + k * math.log(2 * math.pi))


RESEARCH_SCORERS = {"shrinkage_ledoit_wolf": ledoit_wolf_scores, "pca_gaussian": pca_scores}


def research_decisions(scorer, reference: np.ndarray, tests: np.ndarray, folds_max: int,
                       min_reference: int) -> dict:
    """In-sample 5% threshold and cross-conformal decisions of a research scorer.
    `scores` are anomaly scores (higher = more anomalous)."""
    reference, tests = np.asarray(reference, float), np.asarray(tests, float)
    n = len(reference)
    result = {}
    try:
        in_sample = scorer(reference, reference)
        threshold = quantile_type7(in_sample.tolist(), ALPHA[0] / ALPHA[1])
        test_scores = scorer(reference, tests)
        result["in_sample"] = {"status": "decided", "flags": (test_scores <= threshold).tolist(),
                               "scores": (-test_scores).tolist()}
    except np.linalg.LinAlgError as error:
        result["in_sample"] = {"status": "fit_failed", "reason": str(error)}
    if n < min_reference:
        result["cross_conformal"] = {"status": "abstained"}
        return result
    try:
        per_fold = []
        for held in folds(n, folds_max):
            mask = np.ones(n, dtype=bool)
            mask[held] = False
            train = reference[mask]
            per_fold.append((scorer(train, reference[held]).tolist(), scorer(train, tests).tolist()))
        counts = [conformal_count([(held, test[t]) for held, test in per_fold]) for t in range(len(tests))]
        gaps = [min(abs(value - test[t]) for held, test in per_fold for value in held)
                for t in range(len(tests))]
        result["cross_conformal"] = {"status": "decided",
                                     "flags": [conformal_novel(c, n) for c in counts],
                                     "scores": [-conformal_p(c, n) for c in counts],
                                     "counts": counts, "min_gaps": gaps}
    except np.linalg.LinAlgError as error:
        result["cross_conformal"] = {"status": "fit_failed", "reason": str(error)}
    return result
