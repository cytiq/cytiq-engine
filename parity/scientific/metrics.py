"""Independent numerical metrics; no production algorithm or matrix mutation."""
from __future__ import annotations
import numpy as np


def bootstrap_interval(values, statistic=np.mean, *, replicates=1000, seed=42):
    values = np.asarray(values)
    if len(values) < 2 or replicates < 32:
        raise ValueError("confidence intervals need >=2 observations and >=32 replicates")
    rng = np.random.default_rng(seed)
    estimates = [statistic(values[rng.integers(0, len(values), len(values))], axis=0)
                 for _ in range(replicates)]
    return np.quantile(estimates, [0.025, 0.975], axis=0).tolist()


def matrix_perturbation(spillover, latent, perturbation, *, observed=None, inverse=None):
    """Compensate ``latent @ spillover`` with ``spillover + perturbation`` (E).

    The exact error is r = -latent E (S + E)^-1. With A = E S^-1 the
    first-order term f = -latent A satisfies f - r = r A, so its error
    relative to r is at most ||A||_2 (``bound``). ``observed`` and
    ``inverse`` (S^-1) may be passed when they are reused. Returns the
    metrics and r.
    """
    spillover = np.asarray(spillover, dtype=float)
    latent = np.asarray(latent, dtype=float)
    perturbation = np.asarray(perturbation, dtype=float)
    observed = latent @ spillover if observed is None else observed
    inverse = np.linalg.inv(spillover) if inverse is None else inverse
    residual = observed @ np.linalg.inv(spillover + perturbation) - latent
    first_order = -latent @ perturbation @ inverse
    norm = np.linalg.norm(residual)
    return {
        "rmse": float(np.sqrt(np.mean(residual**2))),
        "max_abs": float(np.abs(residual).max()),
        "per_detector_rmse": np.sqrt(np.mean(residual**2, axis=0)).tolist(),
        "first_order_relative_error": float(np.linalg.norm(residual - first_order) / norm) if norm else 0.0,
        "bound": float(np.linalg.norm(perturbation @ inverse, 2)),
    }, residual


def coefficient_perturbations(spillover, latent, deltas=(1e-4, 1e-3, 1e-2)):
    """Every off-diagonal coefficient (row = dye, column = detector) perturbed
    by each delta, through ``matrix_perturbation``. ``rmse_per_unit`` is the
    sensitivity to a unit coefficient error. Nothing here changes a
    production matrix.
    """
    spillover = np.asarray(spillover, dtype=float)
    latent = np.asarray(latent, dtype=float)
    observed = latent @ spillover
    inverse = np.linalg.inv(spillover)
    results = {}
    for dye in range(len(spillover)):
        for detector in range(spillover.shape[1]):
            if dye == detector:
                continue
            cell = results[f"{dye},{detector}"] = {}
            for delta in deltas:
                perturbation = np.zeros_like(spillover)
                perturbation[dye, detector] = delta
                metrics, _ = matrix_perturbation(spillover, latent, perturbation, observed=observed, inverse=inverse)
                cell[str(delta)] = {
                    "rmse": metrics["rmse"],
                    "rmse_per_unit": metrics["rmse"] / delta,
                    "max_abs": metrics["max_abs"],
                    "per_detector_rmse": metrics["per_detector_rmse"],
                    "first_order_relative_error": metrics["first_order_relative_error"],
                    "bound": metrics["bound"],
                }
    return results


def spectral_metrics(reference):
    reference = np.asarray(reference, dtype=float)
    singular = np.linalg.svd(reference, compute_uv=False)
    if not np.isfinite(reference).all() or singular[-1] <= 0:
        raise ValueError("invalid reference matrix")
    unit = reference / np.linalg.norm(reference, axis=0)
    q, r = np.linalg.qr(reference)
    inverse = np.linalg.solve(r, q.T)
    return {"singular_values": singular.tolist(), "condition_number": float(singular[0] / singular[-1]),
            "spectral_cosine": (unit.T @ unit).tolist(),
            "unit_noise_covariance": (inverse @ inverse.T).tolist()}


def empirical_spreading(compensated_positive, compensated_negative, primary_delta):
    """SD excess / sqrt(primary signal), secondary units / sqrt(primary units).

    Negative variance differences remain visible; clipping only defines the
    non-negative reported spreading coefficient.
    """
    positive, negative = np.asarray(compensated_positive), np.asarray(compensated_negative)
    if len(positive) < 3 or len(negative) < 3 or primary_delta <= 0:
        raise ValueError("insufficient independent spreading controls")
    delta = np.var(positive, axis=0, ddof=1) - np.var(negative, axis=0, ddof=1)
    return {"variance_difference": delta.tolist(),
            "spread": (np.sqrt(np.maximum(delta, 0)) / np.sqrt(primary_delta)).tolist()}


def unmixing_spreading(reference, truth, *, seed=101, events=20000, signal=10000.):
    """Independent synthetic controls under a declared ADC variance model.

    Rows are the positive component; columns are the measured unmixed component.
    The Poisson-plus-electronic noise law is known here, not fitted from data.
    No clipping of negative empirical variance differences hides sampling noise.
    """
    reference, truth = np.asarray(reference), np.asarray(truth)
    if reference.shape != truth.shape or np.any(truth < 0) or events < 3 or signal <= 0:
        raise ValueError("invalid spectral spreading experiment")
    q, r = np.linalg.qr(reference)
    inverse = np.linalg.solve(r,q.T)
    rng = np.random.default_rng(seed)
    observed, predicted = [], []
    for column in truth.T:
        mean = signal*column
        positive = rng.poisson(mean/1.5,(events,len(column)))*1.5 + rng.normal(0,40,(events,len(column)))
        negative = rng.normal(0,40,positive.shape)
        delta = np.var(positive@inverse.T,axis=0,ddof=1)-np.var(negative@inverse.T,axis=0,ddof=1)
        observed.append(delta/signal)
        predicted.append(np.diag((inverse*(1.5*mean))@inverse.T)/signal)
    observed, predicted = np.array(observed), np.array(predicted)
    return {"seed":seed,"events_per_control":events,"signal":signal,
            "orientation":"rows=positive component, columns=unmixed component",
            "variance_model":"independent gain=1.5 Poisson ADC plus electronic SD=40",
            "units":"unmixed variance / primary signal; spread is its nonnegative square root",
            "empirical_variance_excess_per_signal":observed.tolist(),
            "predicted_variance_excess_per_signal":predicted.tolist(),
            "empirical_spread":np.sqrt(np.maximum(observed,0)).tolist(),
            "max_absolute_variance_error":float(np.abs(observed-predicted).max()),
            "real_noise_calibration":"not_assessed"}


def retention(labels, stages, technical_good=None):
    """Independent oracle: every stage mask is aligned to original source IDs."""
    labels = np.asarray(labels)
    labelled = np.array([value is not None and str(value) != "NA" for value in labels])
    original_labelled = int(labelled.sum())
    if not original_labelled: raise ValueError("no labelled events")
    keep = np.ones(len(labels), dtype=bool)
    good = None if technical_good is None else np.asarray(technical_good, dtype=bool)
    if good is not None and good.shape != keep.shape:
        raise ValueError("technical truth shape mismatch")
    result = []
    for name, mask in stages:
        mask = np.asarray(mask, dtype=bool)
        if mask.shape != keep.shape:
            raise ValueError("stage mask shape mismatch")
        previous = keep.copy()
        keep &= mask
        populations = {}
        for population in np.unique(labels[labelled]):
            members = labels == population
            original, eligible, kept = int(members.sum()), int((members & previous).sum()), int((members & keep).sum())
            before = original / original_labelled
            denominator = int((keep & labelled).sum())
            after = kept / denominator if denominator else None
            populations[str(population)] = {
                "original": original, "eligible": eligible, "retained": kept,
                "conditional_retention": kept / eligible if eligible else None,
                "retention": kept / original, "frequency_before": before, "frequency_after": after,
                "frequency_error": after - before if after is not None else None,
                "relative_frequency_error": (after - before) / before if after is not None else None,
                "good_retention": float((members & keep & good).sum() / (members & good).sum())
                if good is not None and (members & good).any() else None,
                "technical_removed": int((members & ~keep & ~good).sum()) if good is not None else None,
            }
        result.append({"stage": name, "kept": int(keep.sum()), "newly_removed": int((previous & ~keep).sum()), "populations": populations})
    return result


def wilson_interval(hits, total):
    if total < 1 or not 0 <= hits <= total:
        raise ValueError("invalid binomial counts")
    z = 1.959963984540054
    p = hits / total
    denominator = 1 + z*z/total
    centre = (p + z*z/(2*total)) / denominator
    half = z * np.sqrt(p*(1-p)/total + z*z/(4*total*total)) / denominator
    return [max(0., float(centre-half)), min(1., float(centre+half))]


def solver_study(seed=42, events=128):
    """Offline feasibility only: known count calibration and known AF spectra.

    Oracle weights/second AF are upper bounds on achievable accuracy, not
    estimators usable on arbitrary ADC fluorescence or normalized CyTOF data.
    """
    from scipy.optimize import minimize
    rng = np.random.default_rng(seed)
    matrix = rng.uniform(.02, 1., (16, 5))
    latent = rng.lognormal(3., 1.4, (events, 5))
    expected = latent @ matrix.T + 20.
    observed = rng.poisson(expected).astype(float)
    q, r = np.linalg.qr(matrix)
    ols = np.linalg.solve(r, q.T @ (observed-20.).T).T
    weighted, poisson = [], []
    for y, variance, initial in zip(observed, expected, ols):
        weights = 1 / np.sqrt(variance)
        weighted.append(np.linalg.lstsq(matrix*weights[:, None], (y-20.)*weights, rcond=None)[0])
        def objective(x):
            mean = matrix @ x + 20.
            return float(np.sum(mean-y*np.log(mean))), matrix.T @ (1-y/mean)
        fit = minimize(objective, np.maximum(initial, 0), jac=True, method="L-BFGS-B", bounds=[(0,None)]*5)
        if not fit.success: raise ValueError(f"Poisson feasibility fit failed: {fit.message}")
        poisson.append(fit.x)
    estimates = {"ols": ols, "oracle_wls": np.asarray(weighted), "poisson_nonnegative": np.asarray(poisson)}
    result = {"assumptions": "known independent Poisson counts, known background=20; not an ADC/CyTOF noise calibration", "events": events, "estimators": {}}
    for name, estimate in estimates.items():
        dim = latent < np.quantile(latent, .25, axis=0)
        result["estimators"][name] = {"rmse": float(np.sqrt(np.mean((estimate-latent)**2))), "dim_rmse": float(np.sqrt(np.mean((estimate[dim]-latent[dim])**2))), "negative_fraction": float(np.mean(estimate < 0))}
    second_af = rng.uniform(.02, 1., (16, 1))
    full = np.column_stack([matrix, second_af])
    abundance = np.column_stack([latent, rng.lognormal(3, 1, events)])
    y = abundance @ full.T + rng.normal(0, 1, (events, 16))
    result["multiple_af_oracle"] = {"single_af_rmse": float(np.sqrt(np.mean((np.linalg.lstsq(matrix, y.T, rcond=None)[0].T-latent)**2))), "two_known_af_rmse": float(np.sqrt(np.mean((np.linalg.lstsq(full, y.T, rcond=None)[0].T[:,:5]-latent)**2))), "condition_single": float(np.linalg.cond(matrix)), "condition_multiple": float(np.linalg.cond(full)), "af_estimation_validated": False}
    result["production_solver_added"] = False
    return result


def gaussian_reduction_study(reference, test):
    """Compare shrinkage/PCA on identical Rust features, fitting reference only."""
    reference, test = np.asarray(reference), np.asarray(test)
    centre = reference.mean(axis=0)
    train, held = reference-centre, test-centre
    covariance = train.T @ train / len(train)
    results = {}
    for strength in (.01, .1, 1.):
        shrunk = (1-strength)*covariance + strength*np.diag(np.diag(covariance))
        scale = max(float(np.trace(shrunk)/len(shrunk)), 1e-12)
        shrunk += np.eye(len(shrunk))*scale*1e-8
        inverse = np.linalg.inv(shrunk)
        scores = np.einsum("ij,jk,ik->i", train, inverse, train)
        test_scores = np.einsum("ij,jk,ik->i", held, inverse, held)
        results[f"shrinkage_{strength}"] = float(np.mean(test_scores > np.quantile(scores,.95)))
    _, singular, axes = np.linalg.svd(train, full_matrices=False)
    rank = int(np.sum(singular > np.finfo(float).eps*max(train.shape)*singular[0]))
    dimensions = min(rank, max(1, len(train)//5))
    projection = axes[:dimensions].T
    reduced, reduced_test = train @ projection, held @ projection
    variance = np.maximum(np.var(reduced,axis=0),1e-12)
    results["reference_pca"] = {"dimensions": dimensions, "fpr": float(np.mean(np.sum(reduced_test**2/variance,axis=1) > np.quantile(np.sum(reduced**2/variance,axis=1),.95)))}
    return results


# ---- QC artifact truth (`cytiq.qc.truth.v1`): an independent port of
# `cytiq_qc::artifacts` and the clean-event columns of `cytiq_qc::retention`.
# Counting is vectorized here and per event in Rust; every ratio is one IEEE
# division of exact counts on both sides, so the results must be identical.

TRUTH_CLASSES = ("debris", "doublet", "dead", "time_anomaly", "saturated")
CLASS_TARGETS = {"non_debris": "debris", "singlets": "doublet", "live": "dead",
                 "peacoqc_pass": "time_anomaly", "margin_pass": "saturated",
                 "margin_upper_pass": "saturated"}
CLEAN, ARTIFACT, UNDETERMINED = 0, 1, 2


def base_name(column):
    """`2.2.live` -> `live`: the prefixes `cytiq mask combine` adds."""
    while True:
        index, dot, name = column.partition(".")
        if not dot or not index or any(c not in "0123456789" for c in index):
            return column
        column = name


def tri_state(values):
    """None/False/True -> -1/0/1."""
    return np.array([-1 if value is None else int(bool(value)) for value in values], dtype=np.int8)


def event_states(truth):
    """`truth` maps class -> tri-state codes in raw order."""
    stack = np.stack([truth[name] for name in TRUTH_CLASSES if name in truth])
    artifact = (stack == 1).any(axis=0)
    clean = (stack == 0).all(axis=0)
    return np.where(artifact, ARTIFACT, np.where(clean, CLEAN, UNDETERMINED))


def _ratio(part, whole):
    return part / whole if whole else None


def _share(hit, total, unknown_hit, unknown_miss):
    return {"value": _ratio(hit, total), "lower": _ratio(hit, total + unknown_miss),
            "upper": _ratio(hit + unknown_hit, total + unknown_hit)}


def _purity(wanted, other, unknown):
    every = wanted + other + unknown
    return {"value": _ratio(wanted, wanted + other), "lower": _ratio(wanted, every),
            "upper": _ratio(wanted + unknown, every)}


def _outcomes(codes, selected):
    return {"removed": int(((codes == 0) & selected).sum()), "passed": int(((codes == 1) & selected).sum()),
            "not_assessed": int(((codes == -1) & selected).sum())}


def _kept(outcome):
    return outcome["passed"] + outcome["not_assessed"]


def _total(outcome):
    return outcome["removed"] + _kept(outcome)


def _by_state(codes, states):
    return {"clean": _outcomes(codes, states == CLEAN), "artifact": _outcomes(codes, states == ARTIFACT),
            "undetermined": _outcomes(codes, states == UNDETERMINED)}


def _by_label(codes, label):
    return {"positive": _outcomes(codes, label == 1), "negative": _outcomes(codes, label == 0),
            "unlabelled": _outcomes(codes, label == -1)}


def _target(column, target, codes, labels):
    def not_assessed(reason):
        return {"class": target, "status": "not_assessed", "reason": reason}
    if target not in labels:
        return not_assessed(f"the artifact truth does not label `{target}`")
    label = labels[target]
    if (label == -1).all():
        return not_assessed(f"no event is labelled for `{target}`")
    if (codes == -1).all():
        return not_assessed(f"`{column}` assessed no event")
    outcomes = _by_label(codes, label)
    positive, negative, unknown = outcomes["positive"], outcomes["negative"], outcomes["unlabelled"]
    result = {"class": target, "status": "assessed", "outcomes": outcomes,
              "sensitivity": _share(positive["removed"], _total(positive), unknown["removed"], _kept(unknown))}
    assessed = positive["removed"] + positive["passed"]
    if assessed:
        result["assessed_sensitivity"] = positive["removed"] / assessed
    result["specificity"] = _share(_kept(negative), _total(negative), _kept(unknown), unknown["removed"])
    result["precision"] = _purity(positive["removed"], negative["removed"], unknown["removed"])
    return result


def _raw_rows(source_event_ids, events):
    rows = np.asarray(source_event_ids, dtype=np.int64)
    if len(rows) != events or not np.array_equal(np.sort(rows), np.arange(events)):
        raise ValueError("the mask must hold every raw event exactly once")
    return rows


def artifact_removal_oracle(source_event_ids, keep, filters, truth, events):
    """The `events`, `combined` and `filters` of `qc.json` `artifact_removal`.

    `filters` pairs each mask column with its None/False/True values in mask
    row order; `truth` maps class -> tri-state codes in raw order.
    """
    rows = _raw_rows(source_event_ids, events)
    labels = {name: truth[name][rows] for name in TRUTH_CLASSES if name in truth}
    raw_states = event_states(truth)
    states = raw_states[rows]
    reported = []
    for column, values in filters:
        codes = tri_state(values)
        by_state = _by_state(codes, states)
        clean, artifact, unknown = by_state["clean"], by_state["artifact"], by_state["undetermined"]
        entry = {"column": column, "assessed": int((codes != -1).sum()), "events": by_state,
                 "removed_by_class": {name: int(((codes == 0) & (label == 1)).sum()) for name, label in labels.items()},
                 "clean_loss": _share(clean["removed"], _total(clean), unknown["removed"], _kept(unknown)),
                 "removed_artifact_share": _purity(artifact["removed"], clean["removed"], unknown["removed"])}
        target = CLASS_TARGETS.get(base_name(column))
        if target is not None:
            entry["target"] = _target(column, target, codes, labels)
        reported.append(entry)
    codes = np.where(np.asarray(keep, dtype=bool), 1, 0).astype(np.int8)
    by_state = _by_state(codes, states)
    clean, artifact, unknown = by_state["clean"], by_state["artifact"], by_state["undetermined"]
    kept = int(codes.sum())
    combined = {"kept": kept, "total_retention": _ratio(kept, len(codes)), "events": by_state,
                "artifact_recall": _share(artifact["removed"], _total(artifact), unknown["removed"], _kept(unknown)),
                "clean_retention": _share(_kept(clean), _total(clean), _kept(unknown), unknown["removed"]),
                "removed_artifact_share": _purity(artifact["removed"], clean["removed"], unknown["removed"]),
                "classes": {name: _by_label(codes, label) for name, label in labels.items()}}
    counts = {"raw": events, "clean": int((raw_states == CLEAN).sum()),
              "artifact": int((raw_states == ARTIFACT).sum()),
              "undetermined": int((raw_states == UNDETERMINED).sum()), "clean_of": list(labels)}
    return {"events": counts, "combined": combined, "filters": reported}


def _relative(error, of):
    return error / of if error is not None and of not in (None, 0.0) else None


def membership_retention_oracle(populations, stages, states):
    """`population_retention` stages of `qc.json` for gates over every raw
    event: `populations` pairs names with member flags, `stages` names with
    raw-order keep flags (each applied alone), `states` the event truth."""
    events = len(states)
    base_clean = int((states == CLEAN).sum())
    reported = []
    for stage, keep in stages:
        keep = np.asarray(keep, dtype=bool)
        kept = int(keep.sum())
        rows = []
        for name, members in populations:
            members = np.asarray(members, dtype=bool)
            original, retained = int(members.sum()), int((members & keep).sum())
            original_frequency, retained_frequency = _ratio(original, events), _ratio(retained, kept)
            error = None if retained_frequency is None or original_frequency is None else retained_frequency - original_frequency
            clean = members & (states == CLEAN)
            good_original, good_retained = int(clean.sum()), int((clean & keep).sum())
            clean_frequency = _ratio(good_original, base_clean)
            error_clean = None if retained_frequency is None or clean_frequency is None else retained_frequency - clean_frequency
            rows.append({
                "population": name, "original": original, "eligible": original, "retained": retained,
                "conditional_retention": _ratio(retained, original), "cumulative_retention": _ratio(retained, original),
                "original_frequency": original_frequency, "retained_frequency": retained_frequency,
                "frequency_error": error, "relative_frequency_error": _relative(error, original_frequency),
                "good_original": good_original, "good_retained": good_retained,
                "good_retention": _ratio(good_retained, good_original),
                "technical_removed": int((members & (states == ARTIFACT) & ~keep).sum()),
                "undetermined_removed": int((members & (states == UNDETERMINED) & ~keep).sum()),
                "clean_frequency": clean_frequency, "frequency_error_vs_clean": error_clean,
                "relative_frequency_error_vs_clean": _relative(error_clean, clean_frequency)})
        reported.append({"stage": stage, "input_events": events, "kept_events": kept,
                         "newly_removed": events - kept, "populations": rows})
    return reported
