"""Golden end-to-end dataset for the whole cytiq chain.

Usage: python pipeline.py <out_dir> [ci|full|beads] [--seed N]

One raw sample in which every event's truth is known:

- populations (live singlet lymphocytes and monocytes): CD4 T, CD8 T,
  double-negative T, B, NK, a rare CD19+CD56+ population and monocytes;
- artefacts: debris, doublets, dead cells (bright Live/Dead), an unstable
  acquisition interval (a clog and a gain drift in time) and detector
  saturation (events at `$PnR - 1`).

Fluorescence is generated in the compensated space, mixed with a known
spillover matrix and stored raw, with `$SPILLOVER`, `$CYT` and `$DATE`.

Writes:
- `sample.fcs`
- `truth/events.csv`: `raw_row, population, debris, doublet, dead, unstable,
  margin` (one row per event, flags 0/1; `good` events have no flag)
- `truth/spillover.csv`, `truth/summary.json`
- `gates.xml`: Gating-ML for the populations on `cytiq transform` output
  (arcsinh, cofactor 150) with `uncompensated` dimensions
- `gates_raw.xml`: the same gates for the raw file, compensated through `FCS`
  and transformed with the equivalent `fasinh`, as FlowKit applies them
- `run.yaml`: the run manifest for `cytiq preflight`

Profiles: `ci` (15,000 events, committed to `data/fixtures/pipeline/`),
`full` (200,000 events, parity runs) and `beads` (one discrete population:
degenerate spreads). A profile may also override, for benchmark scenarios
(`parity/scientific/qc_scenarios.py`), the population `shares`, the
`scatter` kind of a population (`small`: rare cells close to debris) and the
dead cells' viability dye `dead_dye` (mean, spread). The overrides keep the
random stream: without them, every profile and seed reproduces its output.
"""

from __future__ import annotations

import argparse
import json
import math
from pathlib import Path

import numpy as np
import pandas as pd

from fcsio import write_fcs

SEED = 20261024
PNR = 262_144
COFACTOR = 150.0
WIDTH_SCALE = 50_000.0
FLUOR = ["FL1-A", "FL2-A", "FL3-A", "FL4-A", "FL5-A", "FL6-A"]
MARKERS = ["CD3", "CD4", "CD8", "CD19", "CD56", "Live/Dead"]
CHANNELS = ["FSC-A", "FSC-H", "FSC-W", "SSC-A", *FLUOR, "Time"]
LABELS = ["", "", "", "", *MARKERS, ""]
# Rows: fluorochrome, columns: detector (fraction of each dye seen in each
# detector), as in `$SPILLOVER`.
SPILL = np.array([
    [1.00, 0.12, 0.03, 0.01, 0.00, 0.02],
    [0.05, 1.00, 0.15, 0.02, 0.01, 0.00],
    [0.01, 0.08, 1.00, 0.10, 0.03, 0.01],
    [0.00, 0.02, 0.06, 1.00, 0.12, 0.02],
    [0.00, 0.01, 0.02, 0.07, 1.00, 0.09],
    [0.02, 0.00, 0.01, 0.02, 0.05, 1.00],
])
# Population: (share of live singlets, scatter kind, positive markers).
POPULATIONS = {
    "cd4_t": (0.32, "lymph", ["CD3", "CD4"]),
    "cd8_t": (0.20, "lymph", ["CD3", "CD8"]),
    "dn_t": (0.05, "lymph", ["CD3"]),
    "b": (0.15, "lymph", ["CD19"]),
    "nk": (0.10, "lymph", ["CD56"]),
    "rare": (0.003, "lymph", ["CD19", "CD56"]),
    "mono": (0.177, "mono", ["CD4"]),
}
POSITIVE = 20_000.0
NEGATIVE = 150.0
# Gate threshold on asinh(x / 150): about 1,500 raw units.
THRESHOLD = 3.0
PROFILES = {
    "ci": {"events": 15_000, "debris": 0.08, "doublet": 0.06, "dead": 0.10,
           "unstable": (0.40, 0.46), "margin": 0.008},
    "full": {"events": 200_000, "debris": 0.08, "doublet": 0.06, "dead": 0.10,
             "unstable": (0.40, 0.46), "margin": 0.008},
}


def _marker_signal(rng: np.random.Generator, positive: list[str], count: int) -> np.ndarray:
    signal = np.empty((count, len(FLUOR)))
    for column, marker in enumerate(MARKERS[:-1]):
        level = POSITIVE if marker in positive else NEGATIVE
        spread = 0.25 * level if marker in positive else 120.0
        signal[:, column] = rng.normal(level, spread, count)
    signal[:, -1] = rng.normal(300.0, 200.0, count)  # live: viability dye dim
    return signal


def _scatter(rng: np.random.Generator, kind: str, count: int) -> tuple[np.ndarray, np.ndarray]:
    if kind == "mono":
        return rng.normal(110_000, 12_000, count), rng.normal(55_000, 9_000, count)
    if kind == "small":  # small, agranular cells whose tail reaches the debris
        return rng.normal(35_000, 6_000, count), rng.normal(12_000, 4_000, count)
    return rng.normal(70_000, 8_000, count), rng.normal(22_000, 5_000, count)


def _cells(rng: np.random.Generator, count: int,
           profile: dict) -> tuple[np.ndarray, dict[str, np.ndarray]]:
    names = list(POPULATIONS)
    overrides = profile.get("shares", {})
    shares = np.array([overrides.get(name, POPULATIONS[name][0]) for name in names])
    population = rng.choice(len(names), count, p=shares / shares.sum())
    fsc = np.empty(count)
    ssc = np.empty(count)
    signal = np.empty((count, len(FLUOR)))
    for index, name in enumerate(names):
        rows = population == index
        _, kind, positive = POPULATIONS[name]
        kind = profile.get("scatter", {}).get(name, kind)
        fsc[rows], ssc[rows] = _scatter(rng, kind, int(rows.sum()))
        signal[rows] = _marker_signal(rng, positive, int(rows.sum()))
    ratio = rng.normal(1.15, 0.03, count)
    return np.array(names)[population], {"fsc": fsc, "ssc": ssc, "ratio": ratio, "signal": signal}


def generate(profile: dict, rng: np.random.Generator) -> tuple[np.ndarray, pd.DataFrame]:
    n = profile["events"]
    population, cells = _cells(rng, n, profile)
    second_population, second = _cells(rng, n, profile)
    fsc_a, ssc, signal = cells["fsc"], cells["ssc"], cells["signal"].copy()
    fsc_h = fsc_a / cells["ratio"]

    roll = rng.random(n)
    debris = roll < profile["debris"]
    doublet = ~debris & (rng.random(n) < profile["doublet"])
    dead = ~debris & ~doublet & (rng.random(n) < profile["dead"])

    # Two cells in one pulse: areas add, height grows only a little.
    fsc_a = np.where(doublet, fsc_a + second["fsc"], fsc_a)
    fsc_h = np.where(doublet, np.maximum(fsc_h, second["fsc"] / second["ratio"]) * 1.1, fsc_h)
    ssc = np.where(doublet, ssc + second["ssc"], ssc)
    signal[doublet] += second["signal"][doublet]

    # Dying cells shrink, get granular and take up the viability dye.
    fsc_a = np.where(dead, fsc_a * 0.85, fsc_a)
    fsc_h = np.where(dead, fsc_h * 0.85, fsc_h)
    ssc = np.where(dead, ssc * 1.2, ssc)
    dye_mean, dye_spread = profile.get("dead_dye", (15_000, 4_000))
    signal[dead, -1] = rng.normal(dye_mean, dye_spread, int(dead.sum()))

    debris_fsc = rng.lognormal(math.log(9_000), 0.45, n)
    fsc_a = np.where(debris, debris_fsc, fsc_a)
    fsc_h = np.where(debris, debris_fsc / rng.normal(1.15, 0.15, n), fsc_h)
    ssc = np.where(debris, rng.lognormal(math.log(6_000), 0.6, n), ssc)
    signal[debris] = rng.normal(500, 1_500, (n, len(FLUOR)))[debris]

    # Unstable acquisition: a clog (signal collapses) followed by a gain drift.
    start, end = (int(fraction * n) for fraction in profile["unstable"])
    middle = (start + end) // 2
    unstable = np.zeros(n, dtype=bool)
    unstable[start:end] = True
    signal[start:middle] *= 0.25
    ramp = 1.0 + 1.5 * np.sin(np.linspace(0, math.pi, end - middle))
    signal[middle:end] *= ramp[:, None]

    observed = signal @ SPILL + rng.normal(0, 40, signal.shape)
    fsc_w = WIDTH_SCALE * fsc_a / np.maximum(fsc_h, 1.0)
    time = np.sort(rng.uniform(0, 600, n)) * 100
    data = np.column_stack([fsc_a, fsc_h, fsc_w, ssc, observed, time])
    data[:, :-1] = np.clip(data[:, :-1], -1_000, PNR - 1)

    # Detector saturation on one random fluorescence channel.
    margin = rng.random(n) < profile["margin"]
    saturated = rng.integers(4, 4 + len(FLUOR), n)
    data[margin, saturated[margin]] = PNR - 1

    population = np.where(debris, "none", population)
    truth = pd.DataFrame({
        "raw_row": np.arange(n),
        "population": population,
        "debris": debris.astype(int),
        "doublet": doublet.astype(int),
        "dead": dead.astype(int),
        "unstable": unstable.astype(int),
        "margin": margin.astype(int),
    })
    del second_population
    return data, truth


def beads(rng: np.random.Generator, n: int = 3_000) -> tuple[np.ndarray, pd.DataFrame]:
    """One bead population on an integer grid: most scatter values repeat."""
    fsc_h = np.round(rng.normal(80_000, 300, n), -3)
    fsc_a = fsc_h.copy()  # area == height: the A/H ratio has zero MAD
    ssc = np.round(rng.normal(30_000, 300, n), -3)
    signal = np.tile([POSITIVE, NEGATIVE, NEGATIVE, NEGATIVE, NEGATIVE, 300.0], (n, 1))
    observed = np.round(signal @ SPILL, -2)
    time = np.sort(rng.uniform(0, 600, n)) * 100
    data = np.column_stack([fsc_a, fsc_h, WIDTH_SCALE * fsc_a / fsc_h, ssc, observed, time])
    truth = pd.DataFrame({"raw_row": np.arange(n), "population": "beads", "debris": 0,
                          "doublet": 0, "dead": 0, "unstable": 0, "margin": 0})
    return data, truth


def _spill_keyword() -> str:
    values = ",".join(f"{value:g}" for value in SPILL.ravel())
    return f"{len(FLUOR)},{','.join(FLUOR)},{values}"


def _gating_ml(raw: bool) -> str:
    """Population gates. On transformed output the dimensions are already
    compensated and on the asinh(x/150) scale; on the raw file the strategy
    compensates through `FCS` and applies the equivalent fasinh, whose output
    is asinh(x/150) / (M ln 10)."""
    m = 4.5
    scale = 1.0 / (m * math.log(10)) if raw else 1.0
    threshold = THRESHOLD * scale
    compensation = "FCS" if raw else "uncompensated"
    transform = ' gating:transformation-ref="asinh"' if raw else ""
    t = COFACTOR * math.sinh(m * math.log(10))

    def fluor(channel: str, low: float | None = None, high: float | None = None) -> str:
        bounds = "".join(
            f' gating:{name}="{value:.10g}"' for name, value in [("min", low), ("max", high)]
            if value is not None
        )
        return (f'    <gating:dimension{bounds} gating:compensation-ref="{compensation}"{transform}>\n'
                f'      <data-type:fcs-dimension data-type:name="{channel}"/>\n'
                f"    </gating:dimension>\n")

    def rect(gate_id: str, parent: str | None, dims: str) -> str:
        parent_attr = f' gating:parent_id="{parent}"' if parent else ""
        return f'  <gating:RectangleGate gating:id="{gate_id}"{parent_attr}>\n{dims}  </gating:RectangleGate>\n'

    cd3, cd4, cd8, cd19, cd56 = FLUOR[:5]
    scatter = (
        '    <gating:dimension gating:min="40000" gating:max="95000" gating:compensation-ref="uncompensated">\n'
        '      <data-type:fcs-dimension data-type:name="FSC-A"/>\n    </gating:dimension>\n'
        '    <gating:dimension gating:max="40000" gating:compensation-ref="uncompensated">\n'
        '      <data-type:fcs-dimension data-type:name="SSC-A"/>\n    </gating:dimension>\n'
    )
    transforms = (
        f'  <transforms:transformation transforms:id="asinh">\n'
        f'    <transforms:fasinh transforms:T="{t:.10g}" transforms:M="{m}" transforms:A="0"/>\n'
        f"  </transforms:transformation>\n" if raw else ""
    )
    return (
        '<?xml version="1.0" encoding="UTF-8"?>\n'
        "<gating:Gating-ML\n"
        '    xmlns:gating="http://www.isac-net.org/std/Gating-ML/v2.0/gating"\n'
        '    xmlns:transforms="http://www.isac-net.org/std/Gating-ML/v2.0/transformations"\n'
        '    xmlns:data-type="http://www.isac-net.org/std/Gating-ML/v2.0/datatypes">\n'
        + transforms
        + rect("Lymphocytes", None, scatter)
        + rect("T", "Lymphocytes", fluor(cd3, low=threshold))
        + rect("CD4T", "T", fluor(cd4, low=threshold) + fluor(cd8, high=threshold))
        + rect("CD8T", "T", fluor(cd8, low=threshold) + fluor(cd4, high=threshold))
        + rect("DNT", "T", fluor(cd4, high=threshold) + fluor(cd8, high=threshold))
        + rect("NonT", "Lymphocytes", fluor(cd3, high=threshold))
        + rect("B", "NonT", fluor(cd19, low=threshold) + fluor(cd56, high=threshold))
        + rect("NK", "NonT", fluor(cd56, low=threshold) + fluor(cd19, high=threshold))
        + rect("Rare", "NonT", fluor(cd19, low=threshold) + fluor(cd56, low=threshold))
        + "</gating:Gating-ML>\n"
    )


MANIFEST = """\
cytometry: conventional
panel:
  name: golden-6c
  markers:
    - { marker: CD3 }
    - { marker: CD4 }
    - { marker: CD8 }
    - { marker: CD19 }
    - { marker: CD56 }
    - { marker: Live/Dead }
  # Built-in artefacts: debris 8%, doublets 6%, dead 10%, an unstable
  # interval (6%) and saturation; each limit leaves headroom above them.
  qc:
    min_events: 10000
    min_kept_events: 8000
    max_removed_fraction: 0.45
    filters:
      margin_pass: { required: true, max_removed_fraction: 0.12 }
      peacoqc_pass: { required: true, max_removed_fraction: 0.2 }
      non_debris: { required: true, max_removed_fraction: 0.12 }
      singlets: { required: true, max_removed_fraction: 0.15 }
      live: { required: true, max_removed_fraction: 0.15 }
samples:
  - { id: GOLDEN, file: sample.fcs }
"""


def write(out: Path, data: np.ndarray, truth: pd.DataFrame) -> None:
    write_fcs(out / "sample.fcs", data, CHANNELS, LABELS, pnr=PNR, extra_keywords={
        "spillover": _spill_keyword(), "cyt": "cytiq golden generator", "date": "24-OCT-2026",
    })
    (out / "truth").mkdir(parents=True, exist_ok=True)
    truth.to_csv(out / "truth" / "events.csv", index=False)
    pd.DataFrame(SPILL, index=FLUOR, columns=FLUOR).to_csv(out / "truth" / "spillover.csv")
    flags = ["debris", "doublet", "dead", "unstable", "margin"]
    good = truth[flags].sum(axis=1) == 0
    summary = {
        "events": int(len(truth)),
        "flags": {flag: int(truth[flag].sum()) for flag in flags},
        "good": int(good.sum()),
        "good_population_counts": {
            name: int(count) for name, count in truth[good]["population"].value_counts().sort_index().items()
        },
    }
    (out / "truth" / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    (out / "gates.xml").write_text(_gating_ml(raw=False))
    (out / "gates_raw.xml").write_text(_gating_ml(raw=True))
    (out / "run.yaml").write_text(MANIFEST)


def main() -> None:
    parser = argparse.ArgumentParser(description="Golden end-to-end dataset")
    parser.add_argument("out", type=Path)
    parser.add_argument("profile", nargs="?", default="ci", choices=[*PROFILES, "beads"])
    parser.add_argument("--seed", type=int, default=SEED)
    args = parser.parse_args()
    rng = np.random.default_rng(args.seed)
    if args.profile == "beads":
        data, truth = beads(rng)
    else:
        data, truth = generate(PROFILES[args.profile], rng)
    write(args.out, data, truth)


if __name__ == "__main__":
    main()
