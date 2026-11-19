"""The pipeline generator's scenario knobs keep the committed fixture exact."""
import hashlib
import sys
import tempfile
import unittest
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "parity/gen"))
import pipeline  # noqa: E402

FIXTURE = ROOT / "data/fixtures/pipeline"
OUTPUTS = ("sample.fcs", "truth/events.csv", "truth/spillover.csv", "truth/summary.json",
           "gates.xml", "gates_raw.xml", "run.yaml")


def digests(directory):
    return {name: hashlib.sha256((directory / name).read_bytes()).hexdigest() for name in OUTPUTS}


def generate(profile, seed=pipeline.SEED):
    return pipeline.generate(profile, np.random.default_rng(seed))


class Generator(unittest.TestCase):
    def test_ci_profile_reproduces_the_committed_fixture(self):
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory)
            pipeline.write(out, *generate(pipeline.PROFILES["ci"]))
            self.assertEqual(digests(out), digests(FIXTURE))

    def test_explicit_defaults_keep_the_random_stream(self):
        data, truth = generate(pipeline.PROFILES["ci"])
        explicit = {**pipeline.PROFILES["ci"], "dead_dye": (15_000, 4_000),
                    "scatter": {"rare": "lymph"},
                    "shares": {name: share for name, (share, _, _) in pipeline.POPULATIONS.items()}}
        same_data, same_truth = generate(explicit)
        np.testing.assert_array_equal(data, same_data)
        self.assertTrue(truth.equals(same_truth))

    def test_knobs_change_only_what_they_name(self):
        data, truth = generate(pipeline.PROFILES["ci"])
        dim, dim_truth = generate({**pipeline.PROFILES["ci"], "dead_dye": (2_000, 800)})
        self.assertTrue(truth.equals(dim_truth))
        dead = truth.dead.to_numpy().astype(bool) & ~truth.margin.to_numpy().astype(bool)
        live_dead = pipeline.CHANNELS.index("FL6-A")
        self.assertLess(np.median(dim[dead, live_dead]), np.median(data[dead, live_dead]))
        unchanged = ~truth.dead.to_numpy().astype(bool)
        np.testing.assert_array_equal(data[unchanged, :4], dim[unchanged, :4])
        _, small = generate({**pipeline.PROFILES["ci"], "scatter": {"rare": "small"}})
        self.assertTrue(truth.population.equals(small.population))

    def test_seed_changes_the_data(self):
        data, _ = generate(pipeline.PROFILES["ci"])
        other, _ = generate(pipeline.PROFILES["ci"], seed=pipeline.SEED + 1)
        self.assertFalse(np.array_equal(data, other))


if __name__ == "__main__":
    unittest.main()
