"""Predeclared scenarios of the QC artifact-truth benchmark (`qc_truth.py`).

Each scenario is the committed `ci` profile of `parity/gen/pipeline.py` with
one technical domain stressed. Scenarios, seeds and the populations reported
as rare are fixed here, before any result: they are never chosen from the
data. Every class's truth is generated (`synthetic`), except saturation,
which is recomputed from the written raw values (`definitional`).
"""
from __future__ import annotations

# The first seed is the committed fixture's: `baseline` must reproduce it.
SEEDS = (20261024, 20261025, 20261026, 20261027, 20261028,
         20261029, 20261030, 20261031, 20261032, 20261033)
CI_SEEDS = SEEDS[:3]

# Populations whose clean-event loss is reported on its own, with Wilson
# intervals: rare by construction (0.3% of live singlets in `ci`).
RARE = ("rare",)

# The dye `weak_cd19` attenuates: CD19 is FL4-A, the fourth column of the
# declared spillover.
CD19 = 3

DOMAINS = ("all", "debris", "doublets", "time", "saturation", "viability",
           "weak_positive", "rare")

# `profile` overrides the `ci` profile; `attenuate` scales one dye's
# compensated signal in the named populations after generation.
SCENARIOS = {
    "baseline": {"domain": "all", "profile": {}},
    "events_50k": {"domain": "all", "profile": {"events": 50_000}},
    "debris_low": {"domain": "debris", "profile": {"debris": 0.02}},
    "debris_high": {"domain": "debris", "profile": {"debris": 0.25}},
    "rare_small": {"domain": "debris",
                   "profile": {"scatter": {"rare": "small"}, "shares": {"rare": 0.01}}},
    "doublet_high": {"domain": "doublets", "profile": {"doublet": 0.15}},
    "time_short": {"domain": "time", "profile": {"unstable": (0.40, 0.41)}},
    "time_long": {"domain": "time", "profile": {"unstable": (0.30, 0.50)}},
    "saturation_high": {"domain": "saturation", "profile": {"margin": 0.03}},
    "dead_high": {"domain": "viability", "profile": {"dead": 0.30}},
    "dead_dim": {"domain": "viability", "profile": {"dead_dye": (2_000, 800)}},
    "weak_cd19": {"domain": "weak_positive", "profile": {},
                  "attenuate": {"populations": ("b", "rare"), "dye": CD19, "factor": 0.05}},
    "rare_0.1pct": {"domain": "rare", "profile": {"shares": {"rare": 0.001}}},
    "rare_1pct": {"domain": "rare", "profile": {"shares": {"rare": 0.01}}},
}

# Mask column of each stage in `mask combine` order (compensate, PeacoQC,
# clean) and the domain whose filter it is.
DOMAIN_COLUMNS = {
    "debris": "non_debris",
    "doublets": "singlets",
    "time": "peacoqc_pass",
    "saturation": "margin_pass",
    "viability": "live",
}
