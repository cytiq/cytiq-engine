"""Predeclared scenarios of the CytoScan novelty benchmark (`novelty_benchmark.py`).

Everything here is fixed before any confirmation result exists: generator
seeds, streams, families, grids, shift levels and replicate counts. Changing
a value changes `DECISION_RULE_SHA256` (the rule embeds `SCENARIOS`), which
the unit tests pin, so a change is a reviewed change and a new rule version.
"""
from __future__ import annotations

import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "parity/gen"))
from cytoscan import ANOMALIES, CHANNELS  # noqa: E402

PRIMARY_SEED = 42
"""Algorithm seed of every decision (`cytoscan --seed`, upstream's 42)."""

STABILITY_SEED_STRIDE = 1 << 32
"""`cytiq_cytoscan::STABILITY_SEED_STRIDE`: seed j is `seed + j * 2^32`."""

ALPHA = (1, 20)
"""Nominal novelty rate as an exact fraction: 1/20 = `DEFAULT_NOVELTY_QUANTILE`."""

DEVELOPMENT_SEEDS = tuple(range(20261201, 20261221))
CI_SEEDS = DEVELOPMENT_SEEDS[:3]
CONFIRMATION_SEEDS = tuple(range(20261301, 20261351))
BOOTSTRAP_SEED = 20261119
SEEDS_USED_ELSEWHERE = (42, 20261022, *range(20261024, 20261034), 20261100, 20261101,
                        20261102, 20262022, 20262023, 20262024, 1, 7, 101, 2026)
"""Seeds of the existing generators and experiments: the benchmark's
generator seeds must be disjoint from them."""

STREAMS = {"reference": 0, "normal": 1, "anomaly": 2, "shifted": 3, "events": 4}
"""`SeedSequence(seed, spawn_key=(stream,))`: one independent stream per role."""

REFERENCE_POOL = 64
NORMAL_TESTS = 40
ANOMALIES_PER_CLASS = 2
EVENTS = 5_000
EVENTS_LARGE = 20_000

SHIFT_LEVELS = {
    "control": {},
    "gain_2.5": {"gain": 0.025},
    "gain_5": {"gain": 0.05},
    "gain_10": {"gain": 0.10},
    "gain_20": {"gain": 0.20},
    "spill_x1.5": {"spill_scale": 1.5},
    "noise_sd_40": {"noise_sd": 40.0},
}
"""Batch effects between the reference (control) batch and normal test files
(`parity/gen/cytoscan.py:Batch`). Every level shares one latent stream, so
levels are paired within a replicate."""

PROFILES = ("ci", "development", "confirmation")
PROFILE_SEEDS = {"ci": CI_SEEDS, "development": DEVELOPMENT_SEEDS,
                 "confirmation": CONFIRMATION_SEEDS}

FAMILIES = {
    "exchangeable": {
        "role": "decision",
        "methods": ("summary", "quantiles"),
        "channels": (6,),
        "sizes": (8, 16, 20, 32, 64),
        "ci_sizes": (16, 32),
        "events": EVENTS,
        "replicates": {"ci": 3, "development": 20, "confirmation": 50},
    },
    "dimension": {
        "role": "decision",
        "methods": ("quantiles",),
        "channels": (2, 4),
        "sizes": (20, 64),
        "events": EVENTS,
        "replicates": {"ci": 0, "development": 15, "confirmation": 30},
    },
    "batch_shift": {
        "role": "descriptive",
        "methods": ("quantiles",),
        "channels": (6,),
        "sizes": (20, 64),
        "events": EVENTS,
        "levels": tuple(SHIFT_LEVELS),
        "replicates": {"ci": 0, "development": 10, "confirmation": 20},
    },
    "aggregate_methods": {
        "role": "descriptive",
        "methods": ("emd", "binning", "fingerprint"),
        "channels": (6,),
        "sizes": (20, 64),
        "events": EVENTS,
        "replicates": {"ci": 0, "development": 5, "confirmation": 10},
    },
    "large_events": {
        "role": "descriptive",
        "methods": ("summary", "quantiles"),
        "channels": (6,),
        "sizes": (20, 32),
        "events": EVENTS_LARGE,
        "replicates": {"ci": 0, "development": 10, "confirmation": 20},
    },
}
"""A family's replicates are the first `replicates[profile]` generator seeds
of the profile; families on 5,000-event files share one replicate's files."""

BASELINE_SEEDS = 10
"""`--stability-seeds` of the baseline runs: seeds 0-4 and 5-9 form the two
independent groups of the seed ensemble."""
STABILITY_SEEDS = 5
"""Seeds over which every arm's flip rate is measured (indices 0-4)."""
EVENT_ARMS = {"events_2500": 2_500, "events_5000": 5_000}
FOLDS_MAX = 10
MIN_CONFORMAL_REFERENCE = 19
"""Smallest reference cohort at which a cross-conformal p-value can reach
1/20: `(1 + 0) / (n + 1) <= 1/20` needs `n >= 19`."""

REAL_DONORS = {
    "fixtures": "data/fixtures/scientific/normal",
    "files": ("sample_2.fcs", "sample_4.fcs", "sample_6.fcs", "sample_8.fcs"),
    "methods": ("summary", "quantiles", "emd", "binning", "fingerprint"),
    "channel_counts": (4, "all_type"),
    "stability_seeds": 5,
}

SCENARIOS = {
    "primary_seed": PRIMARY_SEED, "stability_seed_stride": STABILITY_SEED_STRIDE,
    "alpha": list(ALPHA), "development_seeds": list(DEVELOPMENT_SEEDS),
    "ci_seeds": list(CI_SEEDS), "confirmation_seeds": list(CONFIRMATION_SEEDS),
    "bootstrap_seed": BOOTSTRAP_SEED, "streams": STREAMS, "reference_pool": REFERENCE_POOL,
    "normal_tests": NORMAL_TESTS, "anomalies": list(ANOMALIES),
    "anomalies_per_class": ANOMALIES_PER_CLASS, "channels": list(CHANNELS),
    "shift_levels": SHIFT_LEVELS,
    "families": {name: {key: (list(value) if isinstance(value, tuple) else value)
                        for key, value in family.items()} for name, family in FAMILIES.items()},
    "baseline_seeds": BASELINE_SEEDS, "stability_seeds": STABILITY_SEEDS,
    "event_arms": EVENT_ARMS, "folds_max": FOLDS_MAX,
    "min_conformal_reference": MIN_CONFORMAL_REFERENCE,
    "real_donors": {key: (list(value) if isinstance(value, tuple) else value)
                    for key, value in REAL_DONORS.items()},
}
