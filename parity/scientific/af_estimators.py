"""Autofluorescence (AF) reference estimators for the AF benchmark.

The production section ports crates/cytiq-autospectral/src/spectrum.rs and the
event selection of runner.rs operation by operation. Every sum is a sequential
left fold (`sequential_sum`), never NumPy's pairwise `np.sum`/`mean` or BLAS
`@`/`dot`, because `af_benchmark.py oracle` requires the production spectra to
1e-9. Keep it in step with the Rust code: the CI oracle fails otherwise.

The research section holds the alternative arms. None is used in production;
`af_benchmark.py` measures them against the predeclared `DECISION_RULE`.
"""
from __future__ import annotations

import numpy as np
from scipy.optimize import linear_sum_assignment

# --------------------------------------------------------------------------- production port
HUBER_K = 1.345
RLM_ACCURACY = 1e-4
RLM_MAX_ITERATIONS = 100
RLM_SCALE = 0.6745
SCATTER_NEIGHBOURS = 3
POSITIVITY_THRESHOLD = 0.99
POSITIVE_EVENT_LIMIT = 500
NEGATIVE_EVENT_LIMIT = 1000
MIN_POSITIVE_EVENTS = 20


def sequential_sum(values) -> np.float64:
    """Left-to-right sum, as Rust's `Iterator::sum` (0 for no values)."""
    values = np.asarray(values, dtype=np.float64)
    if values.size == 0:
        return np.float64(0.0)
    return np.add.accumulate(values)[-1]


def median_midpoint(values) -> np.float64:
    """`cytiq_density::median_mut`: the middle value, or the two middle ones' midpoint."""
    ordered = np.sort(np.asarray(values, dtype=np.float64))
    middle = ordered.size // 2
    if ordered.size % 2 == 0:
        return (ordered[middle - 1] + ordered[middle]) / np.float64(2.0)
    return ordered[middle]


def quantile_type7(values, q: float) -> np.float64:
    """runner.rs `quantile`: R's type 7 (not `np.quantile`, whose lerp differs by an ulp)."""
    ordered = np.sort(np.asarray(values, dtype=np.float64))
    if ordered.size == 0:
        return np.float64(0.0)
    position = float(ordered.size - 1) * q
    lower = int(np.floor(position))
    upper = int(np.ceil(position))
    return ordered[lower] + (position - float(lower)) * (ordered[upper] - ordered[lower])


def first_argmax(values, excluded: int | None = None) -> int | None:
    """R's `which.max` without `excluded`: the first index of the largest value."""
    best = None
    for index, value in enumerate(values):
        if index != excluded and (best is None or value > values[best]):
            best = index
    return best


def weighted_line(x: np.ndarray, y: np.ndarray, weights: np.ndarray):
    """spectrum.rs `weighted_line`, with its association: `(w·dx)·dy` and `w·(dx·dx)`."""
    with np.errstate(all="ignore"):
        total = sequential_sum(weights)
        mean_x = sequential_sum(weights * x) / total
        mean_y = sequential_sum(weights * y) / total
        dx = x - mean_x
        covariance = sequential_sum((weights * dx) * (y - mean_y))
        variance = sequential_sum(weights * (dx * dx))
        slope = covariance / variance
        return mean_y - slope * mean_x, slope


def _residuals(x, y, intercept, slope):
    return (y - intercept) - slope * x


def robust_slope(x, y) -> np.float64:
    """`MASS::rlm` slope (Huber, MAD scale, IRLS from least squares), else the `lm` slope."""
    x = np.asarray(x, dtype=np.float64)
    y = np.asarray(y, dtype=np.float64)
    intercept, slope = weighted_line(x, y, np.ones_like(x))
    ols = slope
    residuals = _residuals(x, y, intercept, slope)
    with np.errstate(all="ignore"):
        for _ in range(RLM_MAX_ITERATIONS):
            previous = residuals
            scale = median_midpoint(np.abs(residuals)) / np.float64(RLM_SCALE)
            if scale == 0.0:
                return slope
            weights = np.minimum(HUBER_K / np.abs(residuals / scale), 1.0)
            intercept, slope = weighted_line(x, y, weights)
            residuals = _residuals(x, y, intercept, slope)
            difference = previous - residuals
            change = sequential_sum(difference * difference)
            size = max(sequential_sum(previous * previous), np.float64(1e-20))
            if np.sqrt(change / size) <= RLM_ACCURACY:
                return slope
    return ols


def spectrum(columns, peak: int) -> np.ndarray | None:
    """spectrum.rs `spectrum`: 1 on `peak`, each other detector's robust slope on it,
    divided by the largest value; None when undefined or not positive."""
    x = np.asarray(columns[peak], dtype=np.float64)
    coefficients = np.array([np.float64(1.0) if detector == peak else robust_slope(x, column)
                             for detector, column in enumerate(columns)], dtype=np.float64)
    if not np.all(np.isfinite(coefficients)):
        return None
    largest = np.float64(-np.inf)
    for value in coefficients:
        largest = max(largest, value)
    if not largest > 0.0:
        return None
    return coefficients / largest


def mean_spectrum(Y: np.ndarray) -> np.ndarray:
    """runner.rs `mean_spectrum` over the rows of `Y` (events × detectors)."""
    return np.array([sequential_sum(Y[:, d]) / np.float64(Y.shape[0]) for d in range(Y.shape[1])])


def cosine_sequential(left, right) -> np.float64:
    """runner.rs `cosine`, with sequential sums."""
    left = np.asarray(left, dtype=np.float64)
    right = np.asarray(right, dtype=np.float64)
    return sequential_sum(left * right) / (np.sqrt(sequential_sum(left * left)) * np.sqrt(sequential_sum(right * right)))


def positive_rows(primary: np.ndarray, retained: np.ndarray, negative_primary: np.ndarray) -> np.ndarray:
    """runner.rs `positive_indices`: retained rows above the 0.99 quantile of the negatives'
    primary values, the brightest 500 (ties in row order), returned ascending."""
    threshold = quantile_type7(negative_primary, POSITIVITY_THRESHOLD)
    candidates = retained[primary[retained] > threshold]
    order = np.argsort(-primary[candidates], kind="stable")
    chosen = np.sort(candidates[order[:POSITIVE_EVENT_LIMIT]])
    if chosen.size < MIN_POSITIVE_EVENTS:
        raise ValueError(f"{chosen.size} positive events < {MIN_POSITIVE_EVENTS}")
    return chosen


def scatter_neighbours(positive: np.ndarray, negative: np.ndarray, k: int = SCATTER_NEIGHBOURS) -> np.ndarray:
    """spectrum.rs `scatter_neighbours`: the k nearest negatives of each positive on raw
    FSC/SSC (ties to the lower index), first neighbours of all positives first, unique."""
    k = min(k, negative.shape[0])
    nearest = np.empty((positive.shape[0], k), dtype=np.int64)
    for row, point in enumerate(positive):
        dx = point[0] - negative[:, 0]
        dy = point[1] - negative[:, 1]
        nearest[row] = np.argsort(dx * dx + dy * dy, kind="stable")[:k]
    seen = np.zeros(negative.shape[0], dtype=bool)
    matched = []
    for rank in range(k):
        for index in nearest[:, rank]:
            if not seen[index]:
                seen[index] = True
                matched.append(int(index))
    return np.asarray(matched, dtype=np.int64)


def evenly_spaced(values: np.ndarray, limit: int) -> np.ndarray:
    """spectrum.rs `evenly_spaced`."""
    if len(values) <= limit:
        return np.asarray(values)
    return np.asarray([values[position * len(values) // limit] for position in range(limit)])


def columns_of(Y: np.ndarray) -> list[np.ndarray]:
    """Contiguous detector columns of `Y` (events × detectors)."""
    return [np.ascontiguousarray(Y[:, d]) for d in range(Y.shape[1])]


# --------------------------------------------------------------------------- research arms
# Each arm maps gated AF events Y (events × detectors), their FSC/SSC and a
# seed to (spectra k × detectors, info). Single-AF arms return k = 1.
def _unit_max(vector: np.ndarray) -> np.ndarray:
    return vector / vector.max()


def arm_peak_argmax(Y, scatter=None, seed=1):
    """Production: rlm slopes against the first detector of the highest mean."""
    means = mean_spectrum(Y)
    peak = first_argmax(means)
    result = spectrum(columns_of(Y), peak)
    if result is None:
        raise ValueError("no AF spectrum on the peak detector")
    return result[None, :], {"peak_detector": int(peak)}


def arm_runner_up(Y, scatter=None, seed=1):
    """Production estimator on the runner-up detector (as `af_peak_sensitivity`)."""
    means = mean_spectrum(Y)
    runner_up = first_argmax(means, first_argmax(means))
    result = spectrum(columns_of(Y), runner_up)
    if result is None:
        raise ValueError("no AF spectrum on the runner-up detector")
    return result[None, :], {"peak_detector": int(runner_up)}


def arm_total_signal(Y, scatter=None, seed=1):
    """rlm slopes of every detector against the per-event detector sum, max-normalised.
    No discrete detector choice; the regressor carries each detector's own noise, a
    bias of order var(noise_d)/var(sum)."""
    total = Y.sum(axis=1)
    return _unit_max(np.array([robust_slope(total, column) for column in columns_of(Y)]))[None, :], {}


def arm_mean_ratio(Y, scatter=None, seed=1):
    """The mean spectrum divided by its maximum: the simplest peak-free estimator."""
    return _unit_max(Y.mean(axis=0))[None, :], {}


def arm_principal_axis(Y, scatter=None, seed=1):
    """Top eigenvector of the centred covariance of events between the 1st and 99th
    percentile of total signal; sign with a positive sum; max-normalised."""
    total = Y.sum(axis=1)
    low, high = np.quantile(total, [0.01, 0.99])
    kept = Y[(total >= low) & (total <= high)]
    _, vectors = np.linalg.eigh(np.cov(kept, rowvar=False))
    axis = vectors[:, -1]
    if axis.sum() < 0:
        axis = -axis
    return _unit_max(axis)[None, :], {"events": int(kept.shape[0])}


def _per_group_production(Y, labels, k):
    spectra = []
    counts = []
    for group in range(k):
        rows = Y[labels == group]
        counts.append(int(rows.shape[0]))
        spectra.append(arm_peak_argmax(rows)[0][0] if rows.shape[0] >= 3 else np.full(Y.shape[1], np.nan))
    return np.vstack(spectra), counts


def arm_scatter_strata(k: int):
    """k AF spectra, one per SSC quantile stratum of the gated events (the production
    estimator in each): components identified by an independent variable, scatter."""
    def estimate(Y, scatter, seed=1):
        ssc = scatter[:, 1]
        bounds = [quantile_type7(ssc, i / k) for i in range(1, k)]
        labels = np.searchsorted(np.asarray(bounds), ssc, side="right")
        spectra, counts = _per_group_production(Y, labels, k)
        return spectra, {"events_per_component": counts, "ssc_bounds": [float(b) for b in bounds]}
    return estimate


def spherical_kmeans(units: np.ndarray, k: int, seed: int, iterations: int = 100):
    """Deterministic spherical k-means with k-means++ seeding on unit vectors."""
    rng = np.random.default_rng(seed)
    centres = [units[rng.integers(units.shape[0])]]
    for _ in range(1, k):
        similarity = np.max(units @ np.vstack(centres).T, axis=1)
        distance = np.clip(1.0 - similarity, 0.0, None)
        weights = distance / distance.sum() if distance.sum() > 0 else None
        centres.append(units[rng.choice(units.shape[0], p=weights)])
    centres = np.vstack(centres)
    labels = np.argmax(units @ centres.T, axis=1)
    for _ in range(iterations):
        for group in range(k):
            members = units[labels == group]
            if members.shape[0]:
                centre = members.sum(axis=0)
                centres[group] = centre / np.linalg.norm(centre)
        updated = np.argmax(units @ centres.T, axis=1)
        if np.array_equal(updated, labels):
            break
        labels = updated
    return centres, labels


def arm_spectral_clusters(k: int):
    """k AF spectra from spherical k-means on the unit spectra of events at or above the
    median total signal; every gated event joins its most similar centroid and the
    production estimator runs per cluster. Data-driven: no independent identification."""
    def estimate(Y, scatter, seed=1):
        total = Y.sum(axis=1)
        bright = Y[total >= np.median(total)]
        units = bright / np.linalg.norm(bright, axis=1, keepdims=True)
        centres, _ = spherical_kmeans(units, k, seed)
        norms = np.linalg.norm(Y, axis=1, keepdims=True)
        labels = np.argmax((Y / np.where(norms > 0, norms, 1.0)) @ centres.T, axis=1)
        spectra, counts = _per_group_production(Y, labels, k)
        return spectra, {"events_per_component": counts, "labels": labels}
    return estimate


SINGLE_AF_ARMS = {
    "peak_argmax": arm_peak_argmax,
    "runner_up": arm_runner_up,
    "total_signal": arm_total_signal,
    "mean_ratio": arm_mean_ratio,
    "principal_axis": arm_principal_axis,
}
MULTI_AF_ARMS = {
    "scatter_strata_k2": arm_scatter_strata(2),
    "scatter_strata_k3": arm_scatter_strata(3),
    "spectral_clusters_k2": arm_spectral_clusters(2),
    "spectral_clusters_k3": arm_spectral_clusters(3),
}
BASELINE_ARM = "peak_argmax"
CLUSTER_SEEDS = (1, 2, 3, 4, 5)


# --------------------------------------------------------------------------- solvers
def ols_operator(reference: np.ndarray) -> np.ndarray:
    """P = R⁻¹Qᵀ from the thin QR of the reference (detectors × components)."""
    q, r = np.linalg.qr(reference)
    return np.linalg.solve(r, q.T)


def unmix_joint(Y: np.ndarray, fluorophores: np.ndarray, afs: np.ndarray):
    """OLS with every AF spectrum as its own column: [F | AF₁..AFₖ]."""
    reference = np.column_stack([fluorophores, afs.T])
    abundances = Y @ ols_operator(reference).T
    residual = Y - abundances @ reference.T
    return abundances, np.linalg.norm(residual, axis=1), reference


def unmix_min_residual(Y: np.ndarray, fluorophores: np.ndarray, afs: np.ndarray):
    """Per event, the AF spectrum whose [F | AF_j] fit leaves the smallest residual.
    Returns fluorophore abundances, one AF abundance column, residual norms and the
    assignment. Non-linear: an added fluorophore signal can change the assignment."""
    fits = [unmix_joint(Y, fluorophores, af[None, :]) for af in afs]
    norms = np.column_stack([fit[1] for fit in fits])
    choice = np.argmin(norms, axis=1)
    abundances = np.stack([fit[0] for fit in fits], axis=0)[choice, np.arange(Y.shape[0])]
    return abundances, norms[np.arange(Y.shape[0]), choice], choice


def assign_upstream_score(Y: np.ndarray, fluorophores: np.ndarray, afs: np.ndarray) -> np.ndarray:
    """AutoSpectral's `assign.af.residuals`: the AF spectrum with the largest score
    against the residual of a fluorophore-only fit (`max.col(residual · afᵀ)`)."""
    abundances = Y @ ols_operator(fluorophores).T
    residual = Y - abundances @ fluorophores.T
    return np.argmax(residual @ afs.T, axis=1)


def unmix_assigned(Y, fluorophores, afs, assignment):
    """Unmix each event with [F | AF_assignment]."""
    abundances = np.empty((Y.shape[0], fluorophores.shape[1] + 1))
    residuals = np.empty(Y.shape[0])
    for j, af in enumerate(afs):
        rows = assignment == j
        if rows.any():
            abundances[rows], residuals[rows], _ = unmix_joint(Y[rows], fluorophores, af[None, :])
    return abundances, residuals


def condition_number(matrix: np.ndarray) -> float:
    values = np.linalg.svd(matrix, compute_uv=False)
    return float(values.max() / values.min())


def cosine_matrix(left: np.ndarray, right: np.ndarray) -> np.ndarray:
    """Cosines between the rows of `left` and of `right`."""
    a = left / np.linalg.norm(left, axis=1, keepdims=True)
    b = right / np.linalg.norm(right, axis=1, keepdims=True)
    return a @ b.T


def matched_cosines(estimate: np.ndarray, reference: np.ndarray) -> np.ndarray:
    """Per-component cosine after matching components by Hungarian assignment."""
    similarity = cosine_matrix(estimate, reference)
    rows, columns = linear_sum_assignment(-similarity)
    return similarity[rows, columns]


def largest_principal_angle(estimate: np.ndarray, truth: np.ndarray) -> float:
    """Largest principal angle (radians) from span(estimate rows) to span(truth rows)."""
    qa, _ = np.linalg.qr(estimate.T)
    qb, _ = np.linalg.qr(truth.T)
    cosines = np.linalg.svd(qa.T @ qb, compute_uv=False)
    return float(np.arccos(np.clip(cosines.min(), -1.0, 1.0)))


# --------------------------------------------------------------------------- identifiability
IDENTIFIABILITY = {
    "min_events_per_component": 200,
    "min_event_fraction_per_component": 0.05,
    "max_pairwise_af_cosine": 0.98,
    "max_af_fluorophore_cosine": 0.98,
    "max_condition_ratio_to_production": 1.5,
    "min_split_half_component_cosine": 0.99,
}


def identifiability(afs, counts, fluorophores, production_condition, split_half_min):
    """The predeclared gates a multi-AF arm must pass before it can be eligible."""
    total = sum(counts)
    minimum = max(IDENTIFIABILITY["min_events_per_component"],
                  IDENTIFIABILITY["min_event_fraction_per_component"] * total)
    pairwise = cosine_matrix(afs, afs)[np.triu_indices(afs.shape[0], 1)]
    af_fluor = cosine_matrix(afs, fluorophores.T)
    finite = bool(np.all(np.isfinite(afs)))
    condition = condition_number(np.column_stack([fluorophores, afs.T])) if finite else float("inf")
    checks = {
        "finite_spectra": finite,
        "events_per_component": bool(min(counts) >= minimum),
        "pairwise_af_cosine": bool(finite and pairwise.max() <= IDENTIFIABILITY["max_pairwise_af_cosine"]),
        "af_fluorophore_cosine": bool(finite and af_fluor.max() <= IDENTIFIABILITY["max_af_fluorophore_cosine"]),
        "condition_ratio": bool(condition <= IDENTIFIABILITY["max_condition_ratio_to_production"] * production_condition),
        "split_half_component_cosine": bool(split_half_min >= IDENTIFIABILITY["min_split_half_component_cosine"]),
    }
    return {
        "identifiable": all(checks.values()),
        "checks": checks,
        "events_per_component": [int(c) for c in counts],
        "minimum_events": float(minimum),
        "max_pairwise_af_cosine": float(pairwise.max()) if finite and pairwise.size else None,
        "max_af_fluorophore_cosine": float(af_fluor.max()) if finite else None,
        "condition_number": condition if np.isfinite(condition) else None,
        "split_half_min_component_cosine": float(split_half_min),
    }


# --------------------------------------------------------------------------- summary statistics
def robust_sd(values: np.ndarray) -> float:
    p16, p84 = np.quantile(values, [0.16, 0.84])
    return float((p84 - p16) / 2.0)


def auc(positive: np.ndarray, negative: np.ndarray) -> float:
    """Mann-Whitney AUC: P(positive > negative) + ½P(tie)."""
    from scipy.stats import rankdata
    ranks = rankdata(np.concatenate([positive, negative]))
    n_pos = positive.size
    return float((ranks[:n_pos].sum() - n_pos * (n_pos + 1) / 2.0) / (n_pos * negative.size))
