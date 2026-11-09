"""Generate synthetic cytometry datasets with known ground truth for parity runs.

Usage: python generate.py <out_dir> [full|smoke]

Every dataset is written under <out_dir>/<tool>/ together with a `truth/`
directory holding the values the tools are expected to recover. `smoke`
writes small versions of the datasets whose upstream results are exact or
deterministic (autospill, autospectral, peacoqc, cytoscan_exact, spillqc), for the CI
check in data/fixtures/parity (parity/make_smoke.sh).
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

import numpy as np
import pandas as pd

from cytoscan import gen_cytoscan, gen_cytoscan_exact, gen_cytoscan_ref100
from fcsio import PNR, write_fcs
from gating import gen_gating
import pipeline
from spillqc import gen_spillqc

SEED = 20261022


# --------------------------------------------------------------------------- transform
def gen_transform(out: Path) -> None:
    grid = np.concatenate([
        np.linspace(-1000.0, 0.0, 201),
        np.linspace(0.0, 1000.0, 401)[1:],
        np.geomspace(1000.0, PNR, 400)[1:],
    ])
    data = np.column_stack([np.full_like(grid, 1000.0), np.full_like(grid, 1000.0), grid])
    write_fcs(out / "grid.fcs", data, ["FSC-A", "SSC-A", "X"])
    (out / "truth").mkdir(parents=True, exist_ok=True)
    np.savetxt(out / "truth" / "grid.csv", grid, delimiter=",")


# --------------------------------------------------------------------------- autospill
AUTOSPILL_DYES = ["BV421-A", "FITC-A", "PE-A", "PE-Cy7-A", "APC-A", "APC-Cy7-A"]
AUTOSPILL_SPILLOVER = np.array([
    # rows: dye (primary), columns: detector, same order as AUTOSPILL_DYES
    [1.000, 0.045, 0.010, 0.002, 0.000, 0.000],
    [0.010, 1.000, 0.220, 0.020, 0.003, 0.001],
    [0.000, 0.030, 1.000, 0.150, 0.010, 0.004],
    [0.000, 0.002, 0.040, 1.000, 0.012, 0.080],
    [0.000, 0.000, 0.005, 0.030, 1.000, 0.300],
    [0.000, 0.000, 0.002, 0.120, 0.060, 1.000],
])


def _scatter_cloud(rng: np.random.Generator, n: int, debris_fraction: float) -> tuple[np.ndarray, np.ndarray]:
    n_debris = int(n * debris_fraction)
    n_main = n - n_debris
    fsc_main = rng.normal(90_000, 12_000, n_main)
    ssc_main = 0.6 * fsc_main + rng.normal(0, 7_000, n_main)
    fsc_debris = rng.gamma(2.0, 6_000, n_debris)
    ssc_debris = rng.gamma(2.0, 5_000, n_debris)
    scatter = np.column_stack([np.concatenate([fsc_main, fsc_debris]), np.concatenate([ssc_main, ssc_debris])])
    is_debris = np.concatenate([np.zeros(n_main, bool), np.ones(n_debris, bool)])
    return np.clip(scatter, 0, PNR - 1), is_debris


def _detector_noise(rng: np.random.Generator, signal: np.ndarray, gain: float = 2.0, electronic_sd: float = 60.0) -> np.ndarray:
    # photon-counting (Poisson-like) noise proportional to sqrt(signal) plus electronic noise
    shot = rng.normal(0.0, 1.0, signal.shape) * np.sqrt(gain * np.clip(signal, 0, None))
    return signal + shot + rng.normal(0.0, electronic_sd, signal.shape)


def gen_autospill(out: Path, n_events: int = 20_000, seed: int = SEED + 1) -> None:
    rng = np.random.default_rng(seed)
    (out / "truth").mkdir(parents=True, exist_ok=True)
    rows = []
    autofluorescence = np.array([300.0, 250.0, 120.0, 60.0, 40.0, 30.0])
    for dye_index, dye in enumerate(AUTOSPILL_DYES):
        scatter, is_debris = _scatter_cloud(rng, n_events, 0.15)
        true = np.zeros((n_events, len(AUTOSPILL_DYES)))
        positive = rng.random(n_events) < 0.5
        true[positive, dye_index] = rng.lognormal(np.log(25_000), 0.6, positive.sum())
        signal = true @ AUTOSPILL_SPILLOVER + autofluorescence * rng.lognormal(0, 0.3, (n_events, 1))
        # debris carries random, uncorrelated fluorescence which the scatter gate must exclude
        signal[is_debris] = rng.lognormal(np.log(800), 1.2, (is_debris.sum(), len(AUTOSPILL_DYES)))
        observed = np.clip(_detector_noise(rng, signal), -PNR, PNR - 1)
        data = np.column_stack([scatter, observed])
        name = f"ctrl_{dye.removesuffix('-A').replace('-', '').lower()}.fcs"
        write_fcs(out / name, data, ["FSC-A", "SSC-A", *AUTOSPILL_DYES])
        pd.DataFrame({"debris": is_debris.astype(int)}).to_csv(out / "truth" / f"{name}.debris.csv", index=False)
        rows.append({"filename": name, "dye": dye, "antigen": dye.removesuffix("-A"), "wavelength": 400 + 50 * dye_index})
    pd.DataFrame(rows).to_csv(out / "controls_full.csv", index=False)
    pd.DataFrame(rows)[["filename", "dye"]].to_csv(out / "controls.csv", index=False)
    truth = out / "truth"
    truth.mkdir(parents=True, exist_ok=True)
    pd.DataFrame(AUTOSPILL_SPILLOVER, index=AUTOSPILL_DYES, columns=AUTOSPILL_DYES).to_csv(truth / "spillover.csv")
    pd.DataFrame(np.linalg.inv(AUTOSPILL_SPILLOVER), index=AUTOSPILL_DYES, columns=AUTOSPILL_DYES).to_csv(truth / "compensation.csv")


# --------------------------------------------------------------------------- autospectral
def _gaussian_spectrum(centres: np.ndarray, peak: float, width: float, shoulder: float = 0.0) -> np.ndarray:
    spectrum = np.exp(-0.5 * ((centres - peak) / width) ** 2)
    if shoulder:
        spectrum += shoulder * np.exp(-0.5 * ((centres - peak - 2.5 * width) / (1.5 * width)) ** 2)
    return spectrum / spectrum.max()


def gen_autospectral(out: Path, n_events: int = 20_000, n_sample: int = 30_000) -> None:
    rng = np.random.default_rng(SEED + 2)
    n_det = 16
    detectors = [f"D{i + 1:02d}-A" for i in range(n_det)]
    centres = np.arange(n_det, dtype=float)
    fluors = {
        "BV421": _gaussian_spectrum(centres, 1.0, 1.0, 0.2),
        "BV510": _gaussian_spectrum(centres, 3.0, 1.2, 0.2),
        "FITC": _gaussian_spectrum(centres, 5.0, 1.0, 0.25),
        "PE": _gaussian_spectrum(centres, 8.0, 1.1, 0.3),
        "PE-Cy5": _gaussian_spectrum(centres, 10.5, 1.0, 0.1) + 0.25 * _gaussian_spectrum(centres, 8.0, 1.1),
        "APC": _gaussian_spectrum(centres, 13.0, 1.0, 0.2),
    }
    fluors = {k: v / v.max() for k, v in fluors.items()}
    af = _gaussian_spectrum(centres, 2.0, 3.0)
    names = list(fluors)
    spectra = np.vstack([fluors[n] for n in names])

    def cells(n: int, abundances: np.ndarray) -> np.ndarray:
        scatter, is_debris = _scatter_cloud(rng, n, 0.05)
        af_level = rng.lognormal(np.log(1_500), 0.35, n)
        signal = abundances @ spectra + af_level[:, None] * af[None, :]
        observed = _detector_noise(rng, signal, gain=1.5, electronic_sd=40.0)
        return np.column_stack([scatter, np.clip(observed, -PNR, PNR - 1)]), af_level

    rows = [{"filename": "unstained.fcs", "dye": "Autofluorescence", "primary_channel": ""}]
    data, _ = cells(n_events, np.zeros((n_events, len(names))))
    write_fcs(out / "unstained.fcs", data, ["FSC-A", "SSC-A", *detectors])
    for idx, name in enumerate(names):
        abundance = np.zeros((n_events, len(names)))
        positive = rng.random(n_events) < 0.5
        abundance[positive, idx] = rng.lognormal(np.log(30_000), 0.5, positive.sum())
        data, _ = cells(n_events, abundance)
        file = f"{name.lower().replace('-', '')}.fcs"
        write_fcs(out / file, data, ["FSC-A", "SSC-A", *detectors])
        rows.append({"filename": file, "dye": name, "primary_channel": detectors[int(np.argmax(spectra[idx]))]})
    pd.DataFrame(rows).to_csv(out / "controls.csv", index=False)

    # fully stained sample: each marker independently on/off with lognormal brightness
    on = rng.random((n_sample, len(names))) < 0.4
    abundance = np.where(on, rng.lognormal(np.log(15_000), 0.7, (n_sample, len(names))), 0.0)
    data, af_level = cells(n_sample, abundance)
    write_fcs(out / "sample.fcs", data, ["FSC-A", "SSC-A", *detectors])
    pd.DataFrame([{"filename": "sample.fcs"}]).to_csv(out / "samples.csv", index=False)

    truth = out / "truth"
    truth.mkdir(parents=True, exist_ok=True)
    pd.DataFrame(np.vstack([spectra, af]), index=[*names, "AF"], columns=detectors).to_csv(truth / "spectra.csv")
    pd.DataFrame(np.column_stack([abundance, af_level]), columns=[*names, "AF"]).to_csv(truth / "sample_abundance.csv", index=False)


# --------------------------------------------------------------------------- peacoqc
def gen_peacoqc(out: Path, n_events: int = 200_000) -> None:
    rng = np.random.default_rng(SEED + 3)
    n_ch = 6
    channels = [f"M{i + 1}-A" for i in range(n_ch)]
    # 3 populations on an arcsinh-like scale (PeacoQC is run on transformed data)
    centres = rng.uniform(0.5, 4.0, (3, n_ch))
    weights = np.array([0.5, 0.3, 0.2])
    population = rng.choice(3, n_events, p=weights)
    data = centres[population] + rng.normal(0, 0.25, (n_events, n_ch))
    bad = np.zeros(n_events, bool)

    # artefact 1: clog, signal drops sharply for 4% of the run
    s, e = int(0.20 * n_events), int(0.24 * n_events)
    data[s:e] -= 1.2
    bad[s:e] = True
    # artefact 2: slow drift then recovery over 6% of the run
    s, e = int(0.55 * n_events), int(0.61 * n_events)
    ramp = np.sin(np.linspace(0, np.pi, e - s))[:, None]
    data[s:e] += 1.0 * ramp
    bad[s:e] = ramp[:, 0] > 0.3
    # artefact 3: short burst of noise
    s, e = int(0.85 * n_events), int(0.86 * n_events)
    data[s:e] = rng.uniform(0, 5, (e - s, n_ch))
    bad[s:e] = True

    time = np.sort(rng.uniform(0, 1_000, n_events)) * 100
    fsc = rng.normal(90_000, 10_000, n_events)
    ssc = rng.normal(50_000, 8_000, n_events)
    write_fcs(out / "sample.fcs", np.column_stack([time, fsc, ssc, data]), ["Time", "FSC-A", "SSC-A", *channels])
    truth = out / "truth"
    truth.mkdir(parents=True, exist_ok=True)
    pd.DataFrame({"bad": bad.astype(int)}).to_csv(truth / "bad_mask.csv", index=False)
    (out / "channels.txt").write_text(",".join(channels))


# --------------------------------------------------------------------------- flowsom
def gen_flowsom(out: Path, n_events: int = 100_000, n_dim: int = 10, n_pop: int = 15,
                hard: bool = False) -> None:
    rng = np.random.default_rng(SEED + (5 if hard else 4))
    if hard:
        # overlapping populations with continuous intermediate levels and rare subsets
        centres = rng.uniform(0.5, 3.5, size=(n_pop, n_dim))
        weights = rng.dirichlet(np.full(n_pop, 0.7))
        weights = np.maximum(weights, 0.002)
    else:
        centres = rng.choice([0.3, 1.5, 3.0, 4.2], size=(n_pop, n_dim), p=[0.4, 0.2, 0.25, 0.15])
        weights = rng.dirichlet(np.full(n_pop, 1.5))
        weights = np.maximum(weights, 0.005)
    weights /= weights.sum()
    labels = rng.choice(n_pop, n_events, p=weights)
    scales = rng.uniform(0.35, 0.6, (n_pop, n_dim)) if hard else rng.uniform(0.15, 0.35, (n_pop, n_dim))
    data = centres[labels] + rng.normal(0, 1, (n_events, n_dim)) * scales[labels]
    channels = [f"C{i + 1:02d}-A" for i in range(n_dim)]
    fsc = rng.normal(90_000, 10_000, n_events)
    ssc = rng.normal(50_000, 8_000, n_events)
    write_fcs(out / "sample.fcs", np.column_stack([fsc, ssc, data]), ["FSC-A", "SSC-A", *channels])
    truth = out / "truth"
    truth.mkdir(parents=True, exist_ok=True)
    pd.DataFrame({"label": labels}).to_csv(truth / "labels.csv", index=False)
    (out / "channels.txt").write_text(",".join(channels))


def gen_pipeline(out: Path, profile: str) -> None:
    """Golden end-to-end dataset (see pipeline.py)."""
    rng = np.random.default_rng(pipeline.SEED)
    if profile == "beads":
        data, truth = pipeline.beads(rng)
    else:
        data, truth = pipeline.generate(pipeline.PROFILES[profile], rng)
    pipeline.write(out, data, truth)


FULL = [("transform", gen_transform), ("autospill", gen_autospill), ("autospectral", gen_autospectral),
        ("peacoqc", gen_peacoqc), ("flowsom", gen_flowsom),
        ("flowsom_hard", lambda target: gen_flowsom(target, n_pop=20, hard=True)),
        ("cytoscan_exact", gen_cytoscan_exact), ("cytoscan", gen_cytoscan),
        ("cytoscan_ref100", gen_cytoscan_ref100), ("gating", gen_gating),
        ("pipeline", lambda target: gen_pipeline(target, "full")),
        ("pipeline_beads", lambda target: gen_pipeline(target, "beads")),
        ("spillqc", gen_spillqc)]
# Small enough to commit (about 3 MB), large enough for every gate: the KDE
# scatter gates want a few thousand events, PeacoQC a few hundred bins' worth.
SMOKE = [("autospill", lambda target: gen_autospill(target, n_events=3_000)),
         ("autospectral", lambda target: gen_autospectral(target, n_events=2_000, n_sample=3_000)),
         ("peacoqc", lambda target: gen_peacoqc(target, n_events=15_000)),
         ("cytoscan_exact", gen_cytoscan_exact),
         ("spillqc", lambda target: gen_spillqc(target, n_events=6_000))]


def main() -> None:
    out = Path(sys.argv[1] if len(sys.argv) > 1 else "parity/.data")
    profile = sys.argv[2] if len(sys.argv) > 2 else "full"
    for name, fn in {"full": FULL, "smoke": SMOKE}[profile]:
        target = out / name
        target.mkdir(parents=True, exist_ok=True)
        fn(target)
        print(f"generated {target}")
    (out / "seed.json").write_text(json.dumps({"seed": SEED}))


if __name__ == "__main__":
    main()
