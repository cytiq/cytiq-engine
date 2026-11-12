"""Compute parity metrics between cytiq (Rust), upstream tools and ground truth.

Usage: python compare.py <data_dir> <rust_out> <upstream_out> <metrics_json>
"""

from __future__ import annotations

import json
import os
import re
import sys
from pathlib import Path

import numpy as np
import pandas as pd
from scipy.optimize import linear_sum_assignment
from sklearn.metrics import adjusted_rand_score, normalized_mutual_info_score

sys.path.insert(0, str(Path(__file__).parent / "gen"))
from fcsio import read_fcs  # noqa: E402


def _r(value: float, digits: int = 6) -> float:
    return float(f"{value:.{digits}g}")


def parse_time(text: str) -> tuple[float, float]:
    """Wall-clock seconds and peak RSS in MiB from `/usr/bin/time -l` (BSD,
    bytes) or `/usr/bin/time -v` (GNU, kilobytes) output."""
    bsd = re.search(r"([\d.]+) real", text)
    if bsd:
        rss = int(re.search(r"(\d+)\s+maximum resident set size", text).group(1))
        return float(bsd.group(1)), rss / 2**20
    clock = re.search(r"Elapsed \(wall clock\) time.*?: ([\d:.]+)", text).group(1)
    seconds = 0.0
    for part in clock.split(":"):
        seconds = seconds * 60 + float(part)
    rss_kb = int(re.search(r"Maximum resident set size \(kbytes\): (\d+)", text).group(1))
    return seconds, rss_kb / 2**10


def rust_time(rust: Path, name: str) -> dict:
    seconds, rss_mb = parse_time((rust / f"{name}.time").read_text())
    return {"seconds": seconds, "max_rss_mb": _r(rss_mb, 4)}


def upstream_times(upstream: Path) -> dict:
    timings = pd.read_csv(upstream / "timings.csv", header=None, names=["name", "seconds"])
    return timings.groupby("name")["seconds"].last().to_dict()


# --------------------------------------------------------------------------- transform
def flowkit_reference(x: np.ndarray) -> dict:
    import flowkit as fk

    sample = fk.Sample(pd.DataFrame({"X": x}), sample_id="grid")
    refs = {}
    for key, xform in {
        "logicle": fk.transforms.LogicleTransform(param_t=262144, param_w=0.5, param_m=4.5, param_a=0),
        "hyperlog": fk.transforms.HyperlogTransform(param_t=262144, param_w=0.5, param_m=4.5, param_a=0),
        "asinh_gml": fk.transforms.AsinhTransform(param_t=262144, param_m=4.5, param_a=0),
    }.items():
        refs[key] = xform.apply(x.reshape(-1, 1)).ravel()
    return refs


def compare_transform(data: Path, rust: Path, upstream: Path) -> dict:
    grid = np.loadtxt(data / "transform" / "truth" / "grid.csv", delimiter=",")
    up = pd.read_csv(upstream / "transform.csv")
    fk = flowkit_reference(grid)
    references = {
        "arcsinh": {"flowCore arcsinhTransform(b=1/150)": up["flowcore_arcsinh"].to_numpy()},
        "logicle": {
            "flowCore logicleTransform (0..M decades)": up["flowcore_logicle"].to_numpy(),
            "FlowKit/GatingML logicle (0..1)": fk["logicle"],
        },
        "hyperlog": {
            "flowCore hyperlogtGml2 (0..1)": up["flowcore_hyperlog_gml2"].to_numpy(),
            "FlowKit hyperlog (0..1)": fk["hyperlog"],
        },
        "biexponential": {
            "flowWorkspace flowjo_biexp(256, autospill params)": up["flowjo_biexp_autospill"].to_numpy(),
            "flowWorkspace flowjo_biexp(4096)": up["flowjo_biexp_4096"].to_numpy(),
        },
    }
    out = {}
    for method, refs in references.items():
        values, names = read_fcs(rust / "transform" / f"{method}.fcs")
        rust_values = values[:, names.index("X")]
        entry = {"rust_sample": {str(v): _r(rust_values[np.argmin(abs(grid - v))]) for v in (-1000, 0, 100, 1000, 10000, 262144)},
                 "references": {}}
        for ref_name, ref in refs.items():
            diff = rust_values - ref
            # monotone rescaling check: are the curves the same shape up to an affine map?
            design = np.column_stack([ref, np.ones_like(ref)])
            coef, *_ = np.linalg.lstsq(design, rust_values, rcond=None)
            affine_resid = rust_values - design @ coef
            span = np.ptp(ref)
            entry["references"][ref_name] = {
                "max_abs_diff": _r(np.max(np.abs(diff))),
                "max_abs_diff_rel_to_range": _r(np.max(np.abs(diff)) / span),
                "affine_fit_max_resid_rel_to_range": _r(np.max(np.abs(affine_resid)) / span),
                "ref_sample": {str(v): _r(ref[np.argmin(abs(grid - v))]) for v in (-1000, 0, 100, 1000, 10000, 262144)},
            }
        out[method] = entry
    return out


# --------------------------------------------------------------------------- autospill
def _read_matrix(path: Path, order: list[str]) -> np.ndarray:
    frame = pd.read_csv(path, index_col=0)
    frame.index = [str(i) for i in frame.index]
    return frame.loc[order, order].to_numpy(dtype=float)


def _offdiag(matrix: np.ndarray) -> np.ndarray:
    return matrix[~np.eye(len(matrix), dtype=bool)]


def compare_autospill(data: Path, rust: Path, upstream: Path) -> dict:
    truth_frame = pd.read_csv(data / "autospill" / "truth" / "spillover.csv", index_col=0)
    order = list(truth_frame.index)
    truth = truth_frame.to_numpy()
    rust_m = _read_matrix(rust / "autospill_spillover.csv", order)
    up_m = _read_matrix(upstream / "autospill_spillover.csv", order)

    def stats(a: np.ndarray, b: np.ndarray) -> dict:
        d = _offdiag(a - b)
        comp = np.linalg.inv(a) - np.linalg.inv(b)
        return {"mae": _r(np.mean(np.abs(d))), "max_abs": _r(np.max(np.abs(d))),
                "compensation_frobenius": _r(np.linalg.norm(comp))}

    summary = json.loads((rust / "autospill_debug" / "summary.json").read_text())
    rust_gated = {g["dye"]: g["gated_events"] for g in summary["gate_stats"]}
    gates = pd.read_csv(upstream / "autospill_gates.csv")
    controls = pd.read_csv(data / "autospill" / "controls.csv")
    gate_rows = []
    for _, row in controls.iterrows():
        debris = pd.read_csv(data / "autospill" / "truth" / f"{row.filename}.debris.csv")["debris"].to_numpy().astype(bool)
        r_ids = gates.loc[gates["sample"] == row.dye, "event"].to_numpy() - 1
        gate_rows.append({
            "dye": row.dye, "events": len(debris), "true_non_debris": int((~debris).sum()),
            "rust_gated": rust_gated[row.dye], "upstream_gated": len(r_ids),
            "upstream_debris_in_gate": int(debris[r_ids].sum()),
        })
    convergence = summary.get("extra", {}).get("convergence", {})
    return {
        "rust_vs_truth": stats(rust_m, truth),
        "upstream_vs_truth": stats(up_m, truth),
        "rust_vs_upstream": stats(rust_m, up_m),
        "rust_iterations": convergence.get("iterations"),
        "gates": gate_rows,
    }


# --------------------------------------------------------------------------- spillqc
SPILLQC_NUMERIC = ["secondary_cutoff", "primary_cutoff_neg", "primary_cutoff_pos", "segment_min",
                   "segment_max", "mfi_neg", "sd_neg", "mfi_pos"]


def _flag(values: np.ndarray) -> np.ndarray:
    """CompensAID's PlotMatrix colouring: -1 at SSI <= -1, 1 at SSI >= 1, NaN unassessed."""
    return np.where(np.isnan(values), np.nan, np.where(values <= -1, -1.0, np.where(values >= 1, 1.0, 0.0)))


def spillqc_agreement(upstream: Path, rust: Path) -> dict:
    """cytiq's SSI tables against CompensAID's for one file (directories of
    ssi_matrix.csv, ssi_segments.csv and cutoffs.csv)."""
    r_cut = pd.read_csv(upstream / "cutoffs.csv")
    x_cut = pd.read_csv(rust / "cutoffs.csv")
    cuts = r_cut.merge(x_cut, on="channel", suffixes=("_r", "_x"))
    r_info = pd.read_csv(upstream / "ssi_segments.csv").rename(columns=lambda c: c.replace(".", "_"))
    x_info = pd.read_csv(rust / "ssi_segments.csv")
    key = ["primary_channel", "secondary_channel", "segment"]
    rows = r_info.merge(x_info, on=key, suffixes=("_r", "_x"))

    def max_abs(a: pd.Series, b: pd.Series) -> float:
        both = a.notna() & b.notna()
        return _r(float(np.max(np.abs(a[both].astype(float) - b[both].astype(float))))) if both.any() else 0.0

    na_mismatches = 0
    for column in [*SPILLQC_NUMERIC, "event_count", "event_count_merge", "ssi"]:
        na_mismatches += int((rows[f"{column}_r"].isna() != rows[f"{column}_x"].isna()).sum())
    counts = ["event_count", "event_count_merge"]
    count_mismatches = sum(int((rows[f"{c}_r"].fillna(-1) != rows[f"{c}_x"].fillna(-1)).sum()) for c in counts)
    r_full = pd.read_csv(upstream / "ssi_matrix.csv", index_col=0)
    x_full = pd.read_csv(rust / "ssi_matrix.csv", index_col=0)
    # A channel only one side rates is a disagreement to report, not a KeyError.
    channel_mismatches = sorted(set(r_full.index) ^ set(x_full.index))
    common = [channel for channel in r_full.index if channel in x_full.index]
    r_matrix, x_matrix = r_full.loc[common, common], x_full.loc[common, common]
    off = ~np.eye(len(r_matrix), dtype=bool)
    r_flags, x_flags = _flag(r_matrix.to_numpy(float))[off], _flag(x_matrix.to_numpy(float))[off]
    same = (r_flags == x_flags) | (np.isnan(r_flags) & np.isnan(x_flags))
    return {
        "channel_mismatches": len(channel_mismatches),
        "channels_rated_by_one_side": channel_mismatches,
        "segment_rows": {"upstream": len(r_info), "rust": len(x_info), "matched": len(rows)},
        "cutoff_max_abs": max_abs(cuts["cutoff_r"], cuts["cutoff_x"]),
        "segment_values_max_abs": max(max_abs(rows[f"{c}_r"], rows[f"{c}_x"]) for c in SPILLQC_NUMERIC),
        "ssi_max_abs": max(max_abs(rows["ssi_r"], rows["ssi_x"]),
                           max_abs(r_matrix.stack(), x_matrix.stack())),
        "na_mismatches": na_mismatches,
        "count_mismatches": count_mismatches,
        "message_mismatches": int((rows["message_r"].fillna("") != rows["message_x"].fillna("")).sum()),
        "flag_agreement": _r(float(np.mean(same))),
        "upstream_flags": int(np.sum(np.abs(np.nan_to_num(r_flags)) == 1)),
    }


def _injected_found(matrix: pd.DataFrame, expected: pd.DataFrame) -> int:
    """Injected (primary, secondary) pairs flagged in the expected direction."""
    sign = {"over": -1.0, "under": 1.0}
    found = 0
    for _, row in expected.iterrows():
        value = matrix.loc[row.secondary_channel, row.primary_channel]
        found += int(_flag(np.array([float(value)]))[0] == sign[row.status])
    return found


def compare_spillqc(data: Path, rust: Path, upstream: Path) -> dict:
    expected = pd.read_csv(data / "spillqc" / "truth" / "expected_flags.csv")
    out: dict = {"files": {}}
    for path in sorted((data / "spillqc").glob("*.fcs")):
        name = path.stem
        x_dir, r_dir = rust / "spillqc" / name, upstream / "spillqc" / name
        agreement = spillqc_agreement(r_dir, x_dir)
        agreement["rust_injected_found"] = _injected_found(pd.read_csv(x_dir / "ssi_matrix.csv", index_col=0), expected)
        agreement["upstream_injected_found"] = _injected_found(pd.read_csv(r_dir / "ssi_matrix.csv", index_col=0), expected)
        agreement["injected"] = len(expected)
        out["files"][name] = agreement
    batch = pd.read_csv(rust / "spillqc" / "batch.csv")
    recurrent = 0
    for _, row in expected.iterrows():
        match = batch[(batch.primary_channel == row.primary_channel) & (batch.secondary_channel == row.secondary_channel)]
        recurrent += int((match.status == f"recurrent_{row.status}").any())
    out["batch"] = {"injected_recurrent": recurrent, "injected": len(expected)}
    return out


def compare_autospread(data: Path, rust: Path) -> dict:
    frame = pd.read_csv(rust / "autospread.csv", index_col=0)
    values = frame.to_numpy()
    return {"shape": list(values.shape), "min_offdiag": _r(_offdiag(values).min()), "max_offdiag": _r(_offdiag(values).max()),
            "note": "no upstream reference implementation exists (autospill/AutoSpectral do not compute it)"}


# --------------------------------------------------------------------------- autospectral
def compare_autospectral(data: Path, rust: Path, upstream: Path) -> dict:
    truth = pd.read_csv(data / "autospectral" / "truth" / "spectra.csv", index_col=0)
    fluors = [n for n in truth.index if n != "AF"]
    rust_spec = pd.read_csv(rust / "autospectral" / "references" / "fluorophore_spectra.csv")
    rust_spec = rust_spec.set_index("component")
    detectors = list(truth.columns)
    spec_rows = {}
    for f in fluors:
        t = truth.loc[f, detectors].to_numpy()
        r = rust_spec.loc[f, detectors].to_numpy(dtype=float)
        cos = float(t @ r / np.linalg.norm(t) / np.linalg.norm(r))
        spec_rows[f] = {"cosine": _r(cos), "rust_peak_value": _r(r.max()),
                        "max_abs_diff_after_peak_norm": _r(np.max(np.abs(r / r.max() - t)))}

    abundance = pd.read_csv(data / "autospectral" / "truth" / "sample_abundance.csv")
    unmixed, names = read_fcs(rust / "autospectral" / "unmixed" / "sample.unmixed.fcs")
    up_unmix = pd.read_csv(upstream / "autospectral_unmix_truth_spectra.csv")
    rows = {}
    for f in fluors:
        t = abundance[f].to_numpy()
        r = unmixed[:, names.index(f)]
        u = up_unmix[f].to_numpy()
        slope = float(np.polyfit(t, r, 1)[0])
        rows[f] = {
            "rust_rmse": _r(np.sqrt(np.mean((r - t) ** 2))),
            "rust_scale_vs_truth": _r(slope),
            "rust_pearson": _r(np.corrcoef(r, t)[0, 1]),
            "upstream_ols_truth_spectra_rmse": _r(np.sqrt(np.mean((u - t) ** 2))),
            "upstream_ols_truth_spectra_pearson": _r(np.corrcoef(u, t)[0, 1]),
        }
    cross = upstream / "autospectral_unmix_rust_spectra.csv"
    if cross.exists():
        up_rust = pd.read_csv(cross)
        for f in fluors:
            r = unmixed[:, names.index(f)]
            u = up_rust[f].to_numpy()
            rows[f]["rust_vs_upstream_ols_same_spectra_max_rel"] = _r(np.max(np.abs(r - u)) / np.max(np.abs(u)))
    return {"spectra": spec_rows, "unmixing": rows}


# --------------------------------------------------------------------------- peacoqc
def _mask_metrics(good: np.ndarray, bad_truth: np.ndarray) -> dict:
    removed = ~good
    tp = int((removed & bad_truth).sum())
    fp = int((removed & ~bad_truth).sum())
    fn = int((~removed & bad_truth).sum())
    return {"removed_fraction": _r(removed.mean(), 4),
            "precision": _r(tp / (tp + fp), 4) if tp + fp else None,
            "recall": _r(tp / (tp + fn), 4) if tp + fn else None}


def compare_peacoqc(data: Path, rust: Path, upstream: Path) -> dict:
    bad = pd.read_csv(data / "peacoqc" / "truth" / "bad_mask.csv")["bad"].to_numpy().astype(bool)
    out = {"true_bad_fraction": _r(bad.mean(), 4)}
    for method, r_method in [("all", "all"), ("mad", "mad"), ("it", "it")]:
        mask = pd.read_csv(rust / f"peacoqc_{method}_debug" / "mask.csv")
        rust_good = mask["is_good"].astype(str).str.lower().eq("true").to_numpy()
        up_good = pd.read_csv(upstream / f"peacoqc_{r_method}_mask.csv")["good"].to_numpy().astype(bool)
        r_rem, u_rem = ~rust_good, ~up_good
        union = (r_rem | u_rem).sum()
        summary = json.loads((rust / f"peacoqc_{method}_debug" / "summary.json").read_text())
        out[method] = {
            "rust": _mask_metrics(rust_good, bad),
            "upstream": _mask_metrics(up_good, bad),
            "agreement": _r((rust_good == up_good).mean(), 4),
            "jaccard_removed": _r((r_rem & u_rem).sum() / union, 4) if union else 1.0,
            "rust_n_bins": summary["metrics"].get("n_bins"),
            "rust_events_per_bin": summary["metrics"].get("events_per_bin"),
        }
    return out


# --------------------------------------------------------------------------- flowsom
def _hungarian_accuracy(truth: np.ndarray, pred: np.ndarray) -> float:
    t_ids, t_inv = np.unique(truth, return_inverse=True)
    p_ids, p_inv = np.unique(pred, return_inverse=True)
    table = np.zeros((len(p_ids), len(t_ids)))
    np.add.at(table, (p_inv, t_inv), 1)
    # many-to-one: each predicted cluster maps to its majority truth label (purity)
    return float(table.max(axis=1).sum() / len(truth))


def compare_flowsom(data: Path, rust: Path, upstream: Path, dataset: str = "flowsom") -> dict:
    truth = pd.read_csv(data / dataset / "truth" / "labels.csv")["label"].to_numpy()
    runs = {"rust": pd.read_csv(rust / f"{dataset}.csv")["flowsom_metacluster"].to_numpy()}
    for path in sorted(upstream.glob(f"{dataset}_scale*_seed*.csv")):
        runs[path.stem.removeprefix(f"{dataset}_")] = pd.read_csv(path)["metacluster"].to_numpy()
    out = {"vs_truth": {}, "rust_vs_upstream": {}, "upstream_seed_to_seed": {}}
    for name, labels in runs.items():
        out["vs_truth"][name] = {"ari": _r(adjusted_rand_score(truth, labels), 4),
                                 "nmi": _r(normalized_mutual_info_score(truth, labels), 4),
                                 "purity": _r(_hungarian_accuracy(truth, labels), 4),
                                 "n_clusters": int(len(np.unique(labels)))}
        if name != "rust":
            out["rust_vs_upstream"][name] = {"ari": _r(adjusted_rand_score(runs["rust"], labels), 4),
                                             "nmi": _r(normalized_mutual_info_score(runs["rust"], labels), 4)}
    for scale in ("F", "T"):
        seeds = [runs[k] for k in runs if k.startswith(f"scale{scale}_")]
        aris = [adjusted_rand_score(a, b) for i, a in enumerate(seeds) for b in seeds[i + 1:]]
        out["upstream_seed_to_seed"][f"scale{scale}"] = {"ari_mean": _r(np.mean(aris), 4), "ari_min": _r(np.min(aris), 4)}
    return out


# --------------------------------------------------------------------------- cytoscan
CYTOSCAN_METHODS = ["summary", "quantiles", "emd", "binning", "fingerprint"]


def _flag_metrics(flags: np.ndarray, truth: np.ndarray) -> dict:
    tp = int((flags & truth).sum())
    fp = int((flags & ~truth).sum())
    fn = int((~flags & truth).sum())
    return {"flagged": int(flags.sum()), "tp": tp, "fp": fp, "fn": fn,
            "precision": _r(tp / (tp + fp), 4) if tp + fp else None,
            "recall": _r(tp / (tp + fn), 4) if tp + fn else None}


def _spearman(a: np.ndarray, b: np.ndarray) -> float | None:
    from scipy.stats import spearmanr
    mask = np.isfinite(a) & np.isfinite(b)
    if mask.sum() < 3:
        return None
    return _r(spearmanr(a[mask], b[mask]).statistic, 4)


def compare_cytoscan_dataset(data: Path, rust: Path, upstream: Path, dataset: str) -> dict:
    truth_frame = pd.read_csv(data / dataset / "truth" / "files.csv")
    test_truth = truth_frame[truth_frame.role == "test"].set_index("filename")["is_anomaly"].astype(bool)
    out = {}
    for method in CYTOSCAN_METHODS:
        rust_dir = rust / dataset / method
        up_dir = upstream / dataset
        if not (rust_dir / "cytoscan_summary.csv").exists():
            continue
        rust_features = pd.read_csv(rust_dir / "cytoscan_features.csv").set_index("filename").drop(columns=["path"])
        r_features = pd.read_csv(up_dir / f"features_test_{method}.csv").set_index("filename")
        r_features = r_features.loc[rust_features.index]
        names_match = list(rust_features.columns) == list(r_features.columns)
        delta = np.abs(rust_features.to_numpy() - r_features.to_numpy())
        entry = {"feature_count": int(rust_features.shape[1]), "feature_names_match": names_match,
                 "features_max_abs_diff": _r(np.nanmax(delta)),
                 "features_mean_abs_diff": _r(np.nanmean(delta))}
        seed_band = []
        for seed in (1, 2):
            path = up_dir / f"features_test_{method}_seed{seed}.csv"
            if path.exists():
                other = pd.read_csv(path).set_index("filename").loc[rust_features.index]
                seed_band.append(np.nanmean(np.abs(other.to_numpy() - r_features.to_numpy())))
        if seed_band:
            entry["r_subsample_noise_mean_abs_diff"] = _r(float(np.mean(seed_band)))

        summary = pd.read_csv(rust_dir / "cytoscan_summary.csv").set_index("filename")
        truth = test_truth.loc[summary.index].to_numpy()
        outlier = pd.read_csv(up_dir / f"outlier_{method}.csv").set_index("filename").loc[summary.index]
        r_scores = outlier["score_seed42"].to_numpy()
        rust_scores = summary["outlier_score"].to_numpy(dtype=float)
        rust_out_flags = rust_scores >= 0.5
        r_out_flags = r_scores >= 0.5
        entry["outlier"] = {
            "max_abs_score_diff": _r(np.max(np.abs(rust_scores - r_scores))),
            "spearman": _spearman(rust_scores, r_scores),
            "flag_agreement": _r((rust_out_flags == r_out_flags).mean(), 4),
            "r_isotree_seed_band": _r(float(np.max(np.abs(outlier[["score_seed1042", "score_seed2042"]].to_numpy()
                                                          - r_scores[:, None])))),
            "rust": _flag_metrics(rust_out_flags, truth),
            "upstream": _flag_metrics(r_out_flags, truth),
        }

        rust_ll = summary["novelty_log_likelihood"].to_numpy(dtype=float)
        rust_nov = summary["is_novelty"].astype(str).str.lower().eq("true").to_numpy()
        rust_config = json.loads((rust_dir / "cytoscan_config.json").read_text())["diagnostics"]
        nov = {"rust": {**_flag_metrics(rust_nov, truth), "model": rust_config.get("novelty_model"),
                        "G": rust_config.get("novelty_components")}}
        # `upstream_*`: Mclust on R's features (subsamples differ on large files);
        # `upstream_on_rust_features_*`: Mclust on cytiq's features, the model
        # choice alone.
        for prefix, tag in (("upstream", method), ("upstream_on_rust_features", f"rust_features_{method}")):
            if not (up_dir / f"novelty_{tag}.csv").exists():
                continue
            novelty = pd.read_csv(up_dir / f"novelty_{tag}.csv").set_index("filename").loc[summary.index]
            meta = json.loads((up_dir / f"novelty_{tag}.json").read_text())
            for key in ("default", "VVV"):
                nov[f"{prefix}_{key}"] = _novelty_vs_rust(novelty, meta.get(key, {}), key, rust_ll, rust_nov,
                                                          truth)
        entry["novelty"] = nov
        out[method] = entry
    return out


def _novelty_vs_rust(novelty: pd.DataFrame, info: dict, key: str, rust_ll: np.ndarray, rust_nov: np.ndarray,
                     truth: np.ndarray) -> dict:
    """R's novelty flags and log-densities against cytiq's. CytoScan flags
    `log(dens()) <= threshold`, which is -Inf where dens() underflows; the
    differences use dens(logarithm = TRUE) instead."""
    if info.get("model") is None:
        return {"model": None, "note": "Mclust returned no model"}
    ll = pd.to_numeric(novelty[f"ll_{key}"], errors="coerce").to_numpy(dtype=float)
    column = f"logll_{key}" if f"logll_{key}" in novelty else f"ll_{key}"
    log_ll = pd.to_numeric(novelty[column], errors="coerce").to_numpy(dtype=float)
    r_flags = ll <= info["threshold"]
    finite = np.isfinite(log_ll) & np.isfinite(rust_ll)
    return {
        "model": info.get("model"), "G": info.get("G"),
        "bic": _r(info["bic"]) if info.get("bic") is not None else None,
        "ll_spearman_vs_rust": _spearman(rust_ll[finite], log_ll[finite]),
        "ll_max_abs_diff": _r(float(np.max(np.abs(rust_ll - log_ll)[finite]))) if finite.any() else None,
        "ll_underflow_in_r": int((~np.isfinite(ll)).sum()),
        "flag_agreement_with_rust": _r((rust_nov == r_flags).mean(), 4),
        "flags": _flag_metrics(r_flags, truth),
    }


def compare_cytoscan(data: Path, rust: Path, upstream: Path) -> dict:
    return {dataset: compare_cytoscan_dataset(data, rust, upstream, dataset)
            for dataset in ("cytoscan_exact", "cytoscan", "cytoscan_ref100")
            if (data / dataset).exists() and (rust / dataset).exists()}


def compare_gating(data: Path, rust: Path) -> dict:
    # FlowKit results are recorded by parity/gen/gating.py next to the inputs.
    reference = json.loads((data / "gating" / "flowkit_reference.json").read_text())["gates"]
    rows = {row["gate_id"]: row for row in json.loads((rust / "gating.json").read_text())}
    gates = {
        gate_id: {"flowkit": expected["count"], "rust": rows.get(gate_id, {}).get("count")}
        for gate_id, expected in reference.items()
    }
    matching = sum(1 for gate in gates.values() if gate["flowkit"] == gate["rust"])
    return {"gates": len(gates), "matching_counts": matching, "all_counts_match": matching == len(gates),
            "extra_rust_gates": sorted(set(rows) - set(reference)), "counts": gates}


def compare_gating_quadrant(rust: Path) -> dict:
    """QuadrantGates: `cytiq gate` counts on the committed fixture equal FlowKit,
    and FlowKit reads the Gating-ML cytiq writes back to the same membership."""
    from gating_quadrant import flowkit_reference, validate

    fixtures = Path(__file__).resolve().parents[1] / "data" / "fixtures" / "gating" / "quadrant"
    reference = json.loads((fixtures / "flowkit_reference.json").read_text())["gates"]
    rows = {row["gate_id"]: row for row in json.loads((rust / "gating_quadrant.json").read_text())}
    matching = sum(1 for gate, expected in reference.items()
                   if rows.get(gate, {}).get("count") == expected["count"])
    written = fixtures / "cytiq_written.xml"
    validate(written)
    readback = flowkit_reference(fixtures / "sample.fcs", written)["gates"]
    return {"gates": len(reference), "all_counts_match": matching == len(reference),
            "extra_rust_gates": sorted(set(rows) - set(reference)),
            "written_readback_disagreements": sorted(
                gate for gate in reference if readback.get(gate) != reference[gate])}


# --------------------------------------------------------------------------- pipeline
PIPELINE_FLUOR = ["FL1-A", "FL2-A", "FL3-A", "FL4-A", "FL5-A", "FL6-A"]
# Scatter detectors saturate too; the margin mask judges them with the matrix channels.
PIPELINE_SCATTER = ["FSC-A", "FSC-H", "FSC-W", "SSC-A"]
PIPELINE_FLAGS = ["debris", "doublet", "dead", "unstable", "margin"]
PIPELINE_GATES = {"CD4T": "cd4_t", "CD8T": "cd8_t", "DNT": "dn_t", "B": "b", "NK": "nk", "Rare": "rare"}


def _parquet_by_row(path: Path, rows: int) -> pd.DataFrame:
    import pyarrow.parquet as pq

    table = pq.read_table(path).to_pandas().set_index("source_event_id")
    return table.reindex(np.arange(rows))


def _fcs_columns(path: Path, channels: list[str]) -> np.ndarray:
    data, names = read_fcs(path)
    return data[:, [names.index(channel) for channel in channels]]


def _flowkit_membership(fcs: Path, xml: Path) -> dict[str, np.ndarray]:
    import flowkit as fk

    sample = fk.Sample(str(fcs))
    strategy = fk.parse_gating_xml(str(xml))
    results = strategy.gate_sample(sample)
    return {gate_id: np.asarray(results.get_gate_membership(gate_id, gate_path), dtype=bool)
            for gate_id, gate_path in strategy.get_gate_ids()}


def _recall_loss(predicted: np.ndarray, truth: np.ndarray, good: np.ndarray) -> dict:
    return {"recall": _r((predicted & truth).sum() / max(truth.sum(), 1), 4),
            "good_loss": _r((predicted & good).sum() / max(good.sum(), 1), 4)}


def compare_pipeline_dataset(data: Path, rust: Path, upstream: Path, dataset: str) -> dict:
    d, r, u = data / dataset, rust / dataset, upstream / dataset
    truth = pd.read_csv(d / "truth" / "events.csv")
    rows = len(truth)
    good = (truth[PIPELINE_FLAGS].sum(axis=1) == 0).to_numpy()
    out: dict = {"events": rows}

    # Values: cytiq stores f32, R works in double.
    for stage, fcs, csv in [("compensated", "s.comp.fcs", "r_compensated.csv"),
                            ("transformed", "s.t.fcs", "r_transformed.csv")]:
        mine = _fcs_columns(r / fcs, PIPELINE_FLUOR)
        theirs = pd.read_csv(u / csv).to_numpy()
        scale = np.maximum(np.abs(theirs), 1.0)
        out[f"{stage}_max_rel_delta"] = _r(float(np.max(np.abs(mine - theirs) / scale)))

    # FCS written by cytiq must open in strict readers, without flowio's
    # `ignore_offset_discrepancy`.
    import flowio

    strict = []
    for name in ("s.comp.fcs", "s.t.fcs", "s.peacoqc.fcs"):
        try:
            flowio.FlowData(str(r / name))
            strict.append(True)
        except Exception as error:  # noqa: BLE001 - any parse failure counts
            print(f"{dataset}/{name} is not strictly readable: {error}", file=sys.stderr)
            strict.append(False)
    out["outputs_strictly_readable"] = all(strict)

    raw = _fcs_columns(d / "sample.fcs", PIPELINE_FLUOR + PIPELINE_SCATTER)
    comp_mask = _parquet_by_row(r / "s.comp.masks.parquet", rows)
    margin_removed = ~comp_mask["margin_pass"].astype(bool).to_numpy()
    # The detector limit recomputed independently: `$PnR - 1` only, no low bound.
    at_limit = (raw >= 262_143).any(axis=1)
    r_margins = pd.read_csv(u / "r_margins.csv")["removed"].to_numpy().astype(bool)
    out["margins"] = {
        "agreement_with_detector_limits": _r((margin_removed == at_limit).mean(), 6),
        "agreement_with_RemoveMargins": _r((margin_removed == r_margins).mean(), 4),
        "RemoveMargins_extra_removed": int((r_margins & ~margin_removed).sum()),
        "kept_by_RemoveMargins": int((margin_removed & ~r_margins).sum()),
        **_recall_loss(margin_removed, truth["margin"].to_numpy() == 1, good),
    }

    peacoqc_keep = _parquet_by_row(r / "s.peacoqc.masks.parquet", rows)["keep"].astype(bool).to_numpy()
    r_peacoqc = pd.read_csv(u / "r_peacoqc.csv")["good"].to_numpy().astype(bool)
    out["peacoqc"] = {"agreement": _r((peacoqc_keep == r_peacoqc).mean(), 6),
                      **_recall_loss(~peacoqc_keep, truth["unstable"].to_numpy() == 1, good)}

    clean = _parquet_by_row(r / "clean" / "masks.parquet", rows)
    flowkit_clean = _flowkit_membership(d / "sample.fcs", r / "clean" / "gates.gatingml.xml")
    out["clean"] = {}
    for flag, column in [("debris", "non_debris"), ("doublet", "singlets"), ("dead", "live")]:
        values = clean[column]
        if values.isna().all():
            out["clean"][column] = {"status": "not_assessed"}
            continue
        passed = values.fillna(False).astype(bool).to_numpy()
        out["clean"][column] = {
            "agreement_with_flowkit": _r((passed == flowkit_clean[column]).mean(), 6),
            **_recall_loss(~passed, truth[flag].to_numpy() == 1, good),
        }

    keep = _parquet_by_row(r / "analysis.parquet", rows)["keep"].astype(bool).to_numpy()
    both = (keep & good).sum()
    out["final"] = {"bad_kept_fraction": _r((keep & ~good).sum() / max((~good).sum(), 1), 4),
                    "f1": _r(2 * both / (keep.sum() + good.sum()), 4)}

    if (d / "gates_raw.xml").exists() and (r / "membership.parquet").exists():
        membership = _parquet_by_row(r / "membership.parquet", rows)
        flowkit = _flowkit_membership(d / "sample.fcs", d / "gates_raw.xml")
        out["gates"] = {}
        for gate_id, population in PIPELINE_GATES.items():
            mine = membership[gate_id].fillna(False).astype(bool).to_numpy()
            theirs = flowkit[gate_id] & keep
            truth_members = keep & (truth["population"].to_numpy() == population)
            hits = (mine & truth_members).sum()
            out["gates"][gate_id] = {
                "count": int(mine.sum()), "truth": int(truth_members.sum()),
                "flowkit_disagreement": int((mine != theirs).sum()),
                "precision": _r(hits / max(mine.sum(), 1), 4),
                "recall": _r(hits / max(truth_members.sum(), 1), 4),
            }

    if (u / "r_flowsom.csv").exists():
        flowsom = pd.read_csv(r / "flowsom.csv").set_index("source_event_id")
        theirs = pd.read_csv(u / "r_flowsom.csv").set_index("raw_row")
        population = truth.set_index("raw_row")["population"]
        common = flowsom.index.intersection(theirs.index)
        out["flowsom"] = {
            "rust_ari_vs_truth": _r(adjusted_rand_score(population[flowsom.index],
                                                        flowsom["flowsom_metacluster"]), 4),
            "upstream_ari_vs_truth": _r(adjusted_rand_score(population[theirs.index],
                                                            theirs["metacluster"]), 4),
            "rust_vs_upstream_ari": _r(adjusted_rand_score(flowsom.loc[common, "flowsom_metacluster"],
                                                           theirs.loc[common, "metacluster"]), 4),
            "events_compared": int(len(common)),
        }
    return out


def compare_pipeline(data: Path, rust: Path, upstream: Path) -> dict:
    return {dataset: compare_pipeline_dataset(data, rust, upstream, dataset)
            for dataset in ("pipeline", "pipeline_beads")
            if (data / dataset).exists() and (rust / dataset).exists() and (upstream / dataset).exists()}


# --------------------------------------------------------------------------- tolerances
def _resolve(node, path: list, prefix: tuple = ()):
    if not path:
        yield prefix, node
        return
    head, rest = path[0], path[1:]
    if not isinstance(node, dict):
        return
    keys = node.keys() if head == "*" else ([head] if head in node else [])
    for key in keys:
        yield from _resolve(node[key], rest, (*prefix, key))


def check_tolerances(metrics: dict, rules_path: Path) -> list[str]:
    """Violations of the rules in `tolerances.json` (empty when all pass)."""
    violations = []
    for rule in json.loads(rules_path.read_text())["rules"]:
        matches = list(_resolve(metrics, rule["path"]))
        # An `optional` rule covers datasets that may be skipped (downloads).
        if not matches and not rule.get("optional"):
            violations.append(f"{'.'.join(rule['path'])}: no metric matches")
        for path, value in matches:
            name = ".".join(map(str, path))
            if "equals" in rule:
                failed = value != rule["equals"]
            elif not isinstance(value, (int, float)) or isinstance(value, bool) or value != value:
                failed = True
            else:
                failed = ("min" in rule and value < rule["min"]) or ("max" in rule and value > rule["max"])
            if failed:
                bound = {key: rule[key] for key in ("min", "max", "equals") if key in rule}
                violations.append(f"{name} = {value} violates {bound} ({rule['why']})")
    return violations


def main() -> None:
    data, rust, upstream, target = (Path(p) for p in sys.argv[1:5])
    up_t = upstream_times(upstream)
    metrics = {
        "transform": compare_transform(data, rust, upstream),
        "autospill": compare_autospill(data, rust, upstream),
        "autospread": compare_autospread(data, rust),
        "autospectral": compare_autospectral(data, rust, upstream),
        "peacoqc": compare_peacoqc(data, rust, upstream),
        "flowsom": compare_flowsom(data, rust, upstream),
        "flowsom_hard": compare_flowsom(data, rust, upstream, "flowsom_hard"),
        "cytoscan": compare_cytoscan(data, rust, upstream),
        "gating": compare_gating(data, rust),
        "gating_quadrant": compare_gating_quadrant(rust),
        "pipeline": compare_pipeline(data, rust, upstream),
        "spillqc": compare_spillqc(data, rust, upstream),
        "timing": {
            "autospill": {"rust": rust_time(rust, "autospill"), "upstream_seconds": up_t.get("autospill")},
            "peacoqc_all": {"rust": rust_time(rust, "peacoqc_all"), "upstream_seconds": up_t.get("peacoqc_all")},
            "flowsom": {"rust": rust_time(rust, "flowsom"), "upstream_seconds": up_t.get("flowsom_scaleF_seed1")},
            "flowsom_hard": {"rust": rust_time(rust, "flowsom_hard"), "upstream_seconds": up_t.get("flowsom_hard_scaleF_seed1")},
            "autospectral": {"rust": rust_time(rust, "autospectral")},
            "gating": {"rust": rust_time(rust, "gating")},
            "spillqc": {"rust": rust_time(rust, "spillqc"),
                        "upstream_seconds": sum(v for k, v in up_t.items() if k.startswith("spillqc_")) or None},
        },
    }
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(json.dumps(metrics, indent=2))
    print(json.dumps(metrics, indent=2))
    if os.environ.get("PARITY_SKIP_TOLERANCES"):
        return
    violations = check_tolerances(metrics, Path(__file__).parent / "tolerances.json")
    for violation in violations:
        print(f"TOLERANCE VIOLATION: {violation}", file=sys.stderr)
    if violations:
        sys.exit(1)
    print("all tolerances hold", file=sys.stderr)


if __name__ == "__main__":
    main()
