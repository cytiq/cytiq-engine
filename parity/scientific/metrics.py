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


def coefficient_perturbations(spillover, latent, deltas=(1e-4, 1e-3, 1e-2)):
    """Compensate ``latent @ spillover`` with every off-diagonal coefficient
    (row = dye, column = detector) perturbed by each delta.

    With E the perturbation and A = E S^-1, the exact error is
    r = -latent A (I + A)^-1 and the first-order term f = -latent A, so
    f - r = r A: the first-order term's error relative to r is at most
    ||A||_2 (``bound``). ``rmse_per_unit`` is the sensitivity to a unit
    coefficient error. Nothing here changes a production matrix.
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
                residual = observed @ np.linalg.inv(spillover + perturbation) - latent
                first_order = -latent @ perturbation @ inverse
                norm = np.linalg.norm(residual)
                cell[str(delta)] = {
                    "rmse": float(np.sqrt(np.mean(residual**2))),
                    "rmse_per_unit": float(np.sqrt(np.mean(residual**2)) / delta),
                    "max_abs": float(np.abs(residual).max()),
                    "per_detector_rmse": np.sqrt(np.mean(residual**2, axis=0)).tolist(),
                    "first_order_relative_error": float(np.linalg.norm(residual - first_order) / norm) if norm else 0.0,
                    "bound": float(np.linalg.norm(perturbation @ inverse, 2)),
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
