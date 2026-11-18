"""Synthetic spectral control sets with known autofluorescence (AF) truth.

Built from `parity/gen/generate.py`'s spectra, scatter and detector-noise
models (unchanged, so no fixture moves). Each scenario writes an unstained
control, six single stains with the same AF model, one stained sample, and
`truth/`. These are synthetic: they establish what each estimator recovers
when the AF structure is known, never biological accuracy.
"""
from __future__ import annotations

from pathlib import Path
import sys

import numpy as np
import pandas as pd

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "parity/gen"))
from fcsio import write_fcs  # noqa: E402
from generate import _detector_noise, _gaussian_spectrum, _scatter_cloud  # noqa: E402

SCENARIOS = ("single_af", "two_af_near_tie", "minority_af_scatter_linked",
             "minority_af_scatter_independent", "weak_positive")
SEEDS = (20261101, 20261102, 20261103)
DETECTORS = [f"D{i + 1:02d}-A" for i in range(16)]
CENTRES = np.arange(16, dtype=float)
AF_LEVEL = 1_500.0
AF_LEVEL_SD = 0.35
NOISE = {"gain": 1.5, "electronic_sd": 40.0}


def fluorophore_spectra() -> tuple[list[str], np.ndarray]:
    """`gen_autospectral`'s six fluorophores (rows), max-normalised."""
    fluors = {
        "BV421": _gaussian_spectrum(CENTRES, 1.0, 1.0, 0.2),
        "BV510": _gaussian_spectrum(CENTRES, 3.0, 1.2, 0.2),
        "FITC": _gaussian_spectrum(CENTRES, 5.0, 1.0, 0.25),
        "PE": _gaussian_spectrum(CENTRES, 8.0, 1.1, 0.3),
        "PE-Cy5": _gaussian_spectrum(CENTRES, 10.5, 1.0, 0.1) + 0.25 * _gaussian_spectrum(CENTRES, 8.0, 1.1),
        "APC": _gaussian_spectrum(CENTRES, 13.0, 1.0, 0.2),
    }
    names = list(fluors)
    return names, np.vstack([fluors[n] / fluors[n].max() for n in names])


AF_MAIN = _gaussian_spectrum(CENTRES, 2.0, 3.0)
"""`gen_autospectral`'s AF: one broad source peaking on D03."""
AF_SECOND = _gaussian_spectrum(CENTRES, 6.0, 2.0)
"""A second source peaking on D07, as a different cell type's AF would."""
AF_TIE = (_gaussian_spectrum(CENTRES, 2.0, 1.2), _gaussian_spectrum(CENTRES, 6.0, 1.2))
"""Two narrower sources whose mixture has separate near-equal maxima on D03 and
D07, like Aurora's UV7-A and V7-A on different lasers."""


class Scenario:
    """The AF model of one scenario: sources, population shares and scatter link."""

    def __init__(self, name: str):
        if name not in SCENARIOS:
            raise ValueError(f"unknown scenario {name}")
        self.name = name
        if name == "two_af_near_tie":
            # Two populations whose mean AF signal on D03 and D07 differ by 0.3%.
            self.sources = np.vstack(AF_TIE)
            self.shares = np.array([0.6, 0.4])
            ratio = 1.0 - 0.003
            # 0.6·l1·a1[6] + 0.4·l2·a2[6] = ratio · (0.6·l1·a1[2] + 0.4·l2·a2[2]).
            a1, a2 = AF_TIE
            l2 = 0.6 * AF_LEVEL * (ratio * a1[2] - a1[6]) / (0.4 * (a2[6] - ratio * a2[2]))
            self.levels = np.array([AF_LEVEL, l2])
            self.scatter_shift = np.array([1.0, 1.0])
        elif name.startswith("minority_af"):
            self.sources = np.vstack([AF_MAIN, AF_SECOND])
            self.shares = np.array([0.95, 0.05])
            self.levels = np.array([AF_LEVEL, 3.0 * AF_LEVEL])
            linked = name == "minority_af_scatter_linked"
            self.scatter_shift = np.array([1.0, 1.4 if linked else 1.0])
        else:
            self.sources = AF_MAIN[None, :]
            self.shares = np.array([1.0])
            self.levels = np.array([AF_LEVEL])
            self.scatter_shift = np.array([1.0])

    def cells(self, rng, n: int, fluorophores: np.ndarray, abundances: np.ndarray):
        """FSC, SSC and detector values of `n` cells, their AF population and AF levels."""
        scatter, _ = _scatter_cloud(rng, n, 0.05)
        population = rng.choice(len(self.shares), size=n, p=self.shares)
        scatter[:, 1] = np.clip(scatter[:, 1] * self.scatter_shift[population], 0, 262_143)
        levels = self.levels[population] * rng.lognormal(0.0, AF_LEVEL_SD, n)
        af = levels[:, None] * self.sources[population]
        signal = abundances @ fluorophores + af
        observed = _detector_noise(rng, signal, gain=NOISE["gain"], electronic_sd=NOISE["electronic_sd"])
        return np.column_stack([scatter, np.clip(observed, -262_144, 262_143)]), population, levels


def write_scenario(out: Path, name: str, seed: int, n_control: int = 20_000, n_sample: int = 30_000) -> dict:
    """Write one scenario's controls, sample and truth under `out`; returns its summary."""
    rng = np.random.default_rng(seed)
    scenario = Scenario(name)
    names, fluorophores = fluorophore_spectra()
    out.mkdir(parents=True, exist_ok=False)
    channels = ["FSC-A", "SSC-A", *DETECTORS]
    rows = [{"filename": "unstained.fcs", "dye": "Autofluorescence", "primary_channel": ""}]
    unstained, population, _ = scenario.cells(rng, n_control, fluorophores, np.zeros((n_control, len(names))))
    write_fcs(out / "unstained.fcs", unstained, channels)
    for index, dye in enumerate(names):
        abundance = np.zeros((n_control, len(names)))
        positive = rng.random(n_control) < 0.5
        abundance[positive, index] = rng.lognormal(np.log(30_000), 0.5, positive.sum())
        data, _, _ = scenario.cells(rng, n_control, fluorophores, abundance)
        filename = f"{dye.lower().replace('-', '')}.fcs"
        write_fcs(out / filename, data, channels)
        rows.append({"filename": filename, "dye": dye, "primary_channel": DETECTORS[int(np.argmax(fluorophores[index]))]})
    pd.DataFrame(rows).to_csv(out / "controls.csv", index=False)

    on = rng.random((n_sample, len(names))) < 0.4
    abundance = np.where(on, rng.lognormal(np.log(15_000), 0.7, (n_sample, len(names))), 0.0)
    weak = np.zeros((n_sample, len(names)), dtype=bool)
    if name == "weak_positive":
        # Dim PE (2×) and FITC (3×) populations at multiples of the unmixing spread of
        # unstained cells with the true spectra; those cells carry nothing else.
        spread = _true_unstained_spread(rng, scenario, fluorophores)
        for dye, multiple in (("PE", 2.0), ("FITC", 3.0)):
            index = names.index(dye)
            chosen = rng.random(n_sample) < 0.05
            abundance[chosen] = 0.0
            abundance[chosen, index] = multiple * spread[index]
            weak[chosen, index] = True
    data, sample_population, levels = scenario.cells(rng, n_sample, fluorophores, abundance)
    write_fcs(out / "sample.fcs", data, channels)
    pd.DataFrame([{"filename": "sample.fcs"}]).to_csv(out / "samples.csv", index=False)

    truth = out / "truth"
    truth.mkdir()
    pd.DataFrame(fluorophores, index=names, columns=DETECTORS).to_csv(truth / "spectra.csv")
    pd.DataFrame(scenario.sources, index=[f"AF{i + 1}" for i in range(len(scenario.sources))],
                 columns=DETECTORS).to_csv(truth / "af_spectra.csv")
    table = pd.DataFrame(abundance, columns=names)
    table["af_population"] = sample_population
    table["af_level"] = levels
    table.to_csv(truth / "sample_abundance.csv", index=False)
    pd.DataFrame(weak, columns=names).to_csv(truth / "weak.csv", index=False)
    pd.DataFrame({"af_population": population}).to_csv(truth / "af_membership.csv", index=False)
    means = unstained[:, 2:].mean(axis=0)
    top = np.sort(means)[::-1]
    return {"scenario": name, "seed": seed, "af_sources": int(len(scenario.sources)),
            "unstained_relative_margin": float((top[0] - top[1]) / top[0]),
            "unstained_peak_detector": DETECTORS[int(np.argmax(means))]}


def _true_unstained_spread(rng, scenario: Scenario, fluorophores: np.ndarray) -> np.ndarray:
    """Robust SD per fluorophore of unstained cells unmixed with the true spectra."""
    probe = np.random.default_rng(rng.integers(2**32))
    data, _, _ = scenario.cells(probe, 20_000, fluorophores, np.zeros((20_000, fluorophores.shape[0])))
    reference = np.column_stack([fluorophores.T, scenario.sources.T])
    abundances = np.linalg.lstsq(reference, data[:, 2:].T, rcond=None)[0].T
    p16, p84 = np.quantile(abundances[:, :fluorophores.shape[0]], [0.16, 0.84], axis=0)
    return (p84 - p16) / 2.0
