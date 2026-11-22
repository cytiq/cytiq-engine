"""Synthetic FCS cohorts with known anomalous files for CytoScan parity.

Files follow `CytoScan::generateDemo`: each channel is a two-component normal
mixture on a logicle-like display scale, mapped back to raw intensities. A
`SPILL` keyword is always written because upstream CytoScan compensates every
file with it.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

import numpy as np
import pandas as pd

from fcsio import PNR, write_fcs

CHANNELS = [f"CH{i + 1}-A" for i in range(6)]
SPILL = np.array([
    [1.00, 0.08, 0.02, 0.00, 0.00, 0.00],
    [0.03, 1.00, 0.12, 0.01, 0.00, 0.00],
    [0.00, 0.05, 1.00, 0.10, 0.02, 0.00],
    [0.00, 0.00, 0.04, 1.00, 0.06, 0.01],
    [0.00, 0.00, 0.00, 0.05, 1.00, 0.15],
    [0.00, 0.00, 0.00, 0.01, 0.07, 1.00],
])
ANOMALIES = ["gain_shift", "wrong_spill", "population_loss", "compression", "debris"]


def _spill_keyword(matrix: np.ndarray) -> str:
    values = ",".join(f"{value:.6g}" for value in matrix.ravel())
    return f"{len(CHANNELS)},{','.join(CHANNELS)},{values}"


def _true_signal(rng: np.random.Generator, n: int, proportions: np.ndarray) -> np.ndarray:
    # generateDemo: N(-2, 1) / N(2, 1) mixture per channel on a display scale,
    # mapped to raw intensities with an inverse arcsinh (cofactor 150)
    columns = []
    for proportion in proportions:
        positive = rng.random(n) < proportion
        display = np.where(positive, rng.normal(2.0, 1.0, n), rng.normal(-2.0, 1.0, n))
        columns.append(150.0 * np.sinh(display + 2.5))
    return np.column_stack(columns)


def _file(rng: np.random.Generator, n: int, anomaly: str | None) -> tuple[np.ndarray, np.ndarray]:
    proportions = rng.uniform(0.4, 0.6, len(CHANNELS))
    if anomaly == "population_loss":
        proportions[2] = 0.03
    true = _true_signal(rng, n, proportions)
    if anomaly == "compression":
        true *= 0.3
    if anomaly == "debris":
        debris = rng.random(n) < 0.4
        true[debris] = rng.gamma(1.5, 30.0, (debris.sum(), len(CHANNELS)))
    if anomaly == "gain_shift":
        true[:, 4] *= 4.0
    observed = true @ SPILL + rng.normal(0.0, 20.0, true.shape)
    spill_keyword = np.eye(len(CHANNELS)) if anomaly == "wrong_spill" else SPILL
    return np.clip(observed, -PNR, PNR - 1), spill_keyword


@dataclass(frozen=True)
class Batch:
    """An acquisition batch or instrument relative to the control batch.

    `gain` multiplies every channel's true signal by `1 + gain` (detector
    voltage); `spill_scale` scales the true off-diagonal spillover while the
    written `SPILL` keyword stays nominal (an instrument whose spillover
    differs from its recorded matrix); `noise_sd` is the electronic noise.
    `Batch()` is the control batch: `_file` with its own arithmetic.
    """

    gain: float = 0.0
    spill_scale: float = 1.0
    noise_sd: float = 20.0


def _file_batch(rng: np.random.Generator, n: int, anomaly: str | None,
                batch: Batch) -> tuple[np.ndarray, np.ndarray]:
    """`_file` under `batch`. The random draws come in the same order for every
    batch, so files with the same generator differ only by the batch effect."""
    proportions = rng.uniform(0.4, 0.6, len(CHANNELS))
    if anomaly == "population_loss":
        proportions[2] = 0.03
    true = _true_signal(rng, n, proportions)
    if anomaly == "compression":
        true *= 0.3
    if anomaly == "debris":
        debris = rng.random(n) < 0.4
        true[debris] = rng.gamma(1.5, 30.0, (debris.sum(), len(CHANNELS)))
    if anomaly == "gain_shift":
        true[:, 4] *= 4.0
    true = true * (1.0 + batch.gain)
    identity = np.eye(len(CHANNELS))
    spill = identity + batch.spill_scale * (SPILL - identity)
    observed = true @ spill + rng.normal(0.0, batch.noise_sd, true.shape)
    spill_keyword = identity if anomaly == "wrong_spill" else SPILL
    return np.clip(observed, -PNR, PNR - 1), spill_keyword


def write_files(directory: Path, seed: np.random.SeedSequence, specs: list[tuple[str, str | None]],
                events: int, batch: Batch | None = None) -> None:
    """Write one FCS file per `(name, anomaly)` in `specs`, each from its own
    generator spawned from `seed`, so a file does not depend on the others
    and the same `seed` gives paired files under different batches."""
    batch = batch or Batch()
    for (name, anomaly), child in zip(specs, seed.spawn(len(specs)), strict=True):
        rng = np.random.default_rng(child)
        data, spill = _file_batch(rng, events, anomaly, batch)
        time = np.arange(events, dtype=float)
        fsc = rng.normal(90_000, 9_000, events)
        ssc = rng.normal(50_000, 7_000, events)
        write_fcs(directory / name, np.column_stack([time, fsc, ssc, data]),
                  ["Time", "FSC-A", "SSC-A", *CHANNELS], extra_keywords={"spill": _spill_keyword(spill)})


def write_cohort(out: Path, seed: int, n_test: int, n_reference: int, events: int,
                 anomalous_test: dict[int, str]) -> None:
    rng = np.random.default_rng(seed)
    rows = []
    for role, count in (("reference", n_reference), ("test", n_test)):
        for index in range(count):
            anomaly = anomalous_test.get(index) if role == "test" else None
            data, spill = _file(rng, events, anomaly)
            time = np.arange(events, dtype=float)
            fsc = rng.normal(90_000, 9_000, events)
            ssc = rng.normal(50_000, 7_000, events)
            name = f"{role}_{index + 1:03d}.fcs"
            write_fcs(out / role / name, np.column_stack([time, fsc, ssc, data]),
                      ["Time", "FSC-A", "SSC-A", *CHANNELS], extra_keywords={"spill": _spill_keyword(spill)})
            rows.append({"role": role, "filename": name, "anomaly": anomaly or "", "is_anomaly": int(anomaly is not None)})
    (out / "truth").mkdir(parents=True, exist_ok=True)
    pd.DataFrame(rows).to_csv(out / "truth" / "files.csv", index=False)
    (out / "channels.txt").write_text(",".join(CHANNELS))


def gen_cytoscan_exact(out: Path) -> None:
    # 1000 events and 10 files per cohort: R's which.lines = 1000 and
    # nAgg = ceiling(10000 / 10) = 1000 both keep every event, so features are
    # free of subsampling randomness on both sides.
    write_cohort(out, seed=20261100, n_test=10, n_reference=10, events=1_000,
                 anomalous_test={1: "gain_shift", 4: "compression", 7: "wrong_spill"})


def gen_cytoscan(out: Path) -> None:
    anomalies = {2: "gain_shift", 6: "wrong_spill", 10: "population_loss", 14: "compression", 18: "debris"}
    write_cohort(out, seed=20261101, n_test=20, n_reference=20, events=20_000, anomalous_test=anomalies)


def gen_cytoscan_ref100(out: Path) -> None:
    anomalies = {2: "gain_shift", 6: "wrong_spill", 10: "population_loss", 14: "compression", 18: "debris"}
    write_cohort(out, seed=20261102, n_test=20, n_reference=100, events=20_000, anomalous_test=anomalies)
