"""CytoScan novelty benchmark: frozen rule, statistics and generator."""
import hashlib
import json
import sys
import tempfile
import unittest
from argparse import Namespace
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "parity/gen"))
import cytoscan  # noqa: E402
import novelty_arms as arms  # noqa: E402
import novelty_benchmark as nb  # noqa: E402
import novelty_scenarios as sc  # noqa: E402

PINNED_RULE_SHA256 = "7a643c44652192f5d8381ff31e2521255a93b216379dab1bd0979b0b31219f34"


class FrozenRule(unittest.TestCase):
    def test_decision_rule_is_pinned(self):
        # A changed rule, grid or seed is a new rule version: bump
        # DECISION_RULE["version"], review, then update this pin.
        self.assertEqual(nb.DECISION_RULE_SHA256, PINNED_RULE_SHA256)
        self.assertEqual(nb.DECISION_RULE["version"], 2)
        self.assertEqual(nb.DECISION_RULE["history"][0]["from_sha256"], nb.RULE_V1_SHA256)

    def test_seed_sets_are_disjoint(self):
        development, confirmation = set(sc.DEVELOPMENT_SEEDS), set(sc.CONFIRMATION_SEEDS)
        self.assertFalse(development & confirmation)
        self.assertLessEqual(set(sc.CI_SEEDS), development)
        self.assertFalse((development | confirmation) & set(sc.SEEDS_USED_ELSEWHERE))
        self.assertNotIn(sc.BOOTSTRAP_SEED, development | confirmation)
        for family in sc.FAMILIES.values():
            for profile, count in family["replicates"].items():
                self.assertLessEqual(count, len(sc.PROFILE_SEEDS[profile]))

    def test_alpha_is_the_production_novelty_quantile(self):
        self.assertEqual(sc.ALPHA[0] / sc.ALPHA[1], 0.05)
        # (1 + 0) / (n + 1) <= 1/20 first holds at n = 19.
        self.assertFalse(arms.conformal_novel(0, sc.MIN_CONFORMAL_REFERENCE - 1))
        self.assertTrue(arms.conformal_novel(0, sc.MIN_CONFORMAL_REFERENCE))


class Seeds(unittest.TestCase):
    def test_stability_seed_matches_the_rust_formula(self):
        self.assertEqual(arms.stability_seed(42, 0), 42)
        self.assertEqual(arms.stability_seed(42, 1), 42 + 2 ** 32)
        self.assertEqual(arms.stability_seed(2 ** 64 - 1, 1), 2 ** 32 - 1)
        with self.assertRaises(ValueError):
            arms.stability_seed(42, 64)

    def test_derived_seeds_share_no_tree(self):
        seeds = [arms.stability_seed(42, j) for j in range(64)]
        trees = {(s + t) % 2 ** 64 for s in seeds for t in range(1000)}
        self.assertEqual(len(trees), 64 * 1000)


class Conformal(unittest.TestCase):
    def test_counts_include_ties_and_infinities(self):
        folds = [([-3.0, -1.0], -2.0), ([-2.0, float("-inf")], -2.0)]
        # fold 0: -3 <= -2; fold 1: -2 <= -2 (tie) and -inf <= -2
        self.assertEqual(arms.conformal_count(folds), 3)
        self.assertEqual(arms.conformal_count([([-1.0], float("-inf"))]), 0)

    def test_integer_rule(self):
        cases = {(18, 0): False, (19, 0): True, (19, 1): False,
                 (39, 0): True, (39, 1): True, (39, 2): False}
        for (n, count), novel in cases.items():
            self.assertEqual(arms.conformal_novel(count, n), novel, (n, count))
            self.assertEqual(arms.conformal_p(count, n) <= 0.05 + 1e-15, novel)

    def test_folds_partition_the_reference_by_index(self):
        twenty = arms.folds(20, 10)
        self.assertEqual(twenty[3], [3, 13])
        self.assertEqual(sorted(i for fold in twenty for i in fold), list(range(20)))
        self.assertEqual(len(arms.folds(5, 10)), 5)
        with self.assertRaises(ValueError):
            arms.folds(1, 10)

    def test_split_conformal_reference(self):
        reference = arms.split_conformal_reference(19, 40)
        self.assertEqual(reference["k"], 1)
        self.assertAlmostEqual(reference["mean_fpr"], 0.05)
        self.assertAlmostEqual(reference["p_fpr_gt_limit"], 0.131434, places=5)
        self.assertAlmostEqual(arms.split_conformal_reference(64, 40)["p_fpr_gt_limit"], 0.074887, places=5)
        self.assertTrue(arms.split_conformal_reference(18, 40)["abstains"])


class Agreement(unittest.TestCase):
    def test_pairwise_disagreement_matches_rust(self):
        # stability.rs: pairwise_disagreement_counts_disagreeing_seed_pairs
        self.assertEqual(arms.pairwise_disagreement([(2, 5), (0, 5), (5, 5), (1, 5)]), 10 / 40)
        self.assertIsNone(arms.pairwise_disagreement([(0, 1)]))

    def test_fleiss_kappa(self):
        # 4 files x 3 seeds: flagged counts 3, 0, 2, 0
        flags = np.array([[1, 1, 1], [0, 0, 0], [1, 1, 0], [0, 0, 0]], dtype=bool)
        agreement = (1 + 1 + (2 * 1 + 1 * 0) / 6 + 1) / 4
        share = 5 / 12
        expected = share ** 2 + (1 - share) ** 2
        self.assertAlmostEqual(arms.fleiss_kappa(flags), (agreement - expected) / (1 - expected))
        self.assertIsNone(arms.fleiss_kappa(np.zeros((4, 3), dtype=bool)))
        self.assertIsNone(arms.fleiss_kappa(np.ones((4, 1), dtype=bool)))

    def test_flip_metrics_on_normals_only(self):
        seeds = [{"a": True, "b": False, "c": True}, {"a": True, "b": True, "c": False}]
        metrics = nb.flip_metrics(seeds, ["a", "b", "c"], {"b"})
        self.assertAlmostEqual(metrics["flip_all"], 2 / 3)
        self.assertEqual(metrics["flip_normals"], 1.0)
        self.assertAlmostEqual(metrics["unanimous"], 1 / 3)


class Statistics(unittest.TestCase):
    def test_bootstrap_is_deterministic(self):
        values = [0.0, 0.1, 0.05, 0.2, 0.0]
        first = arms.bootstrap_mean_interval(values, 2000, 20261119)
        self.assertEqual(first, arms.bootstrap_mean_interval(values, 2000, 20261119))
        self.assertLessEqual(first[0], np.mean(values) + 1e-12)
        self.assertGreaterEqual(first[1], np.mean(values) - 1e-12)
        low, high = arms.bootstrap_mean_interval([0.05] * 7, 500, 1)
        self.assertEqual(low, high)
        self.assertAlmostEqual(low, 0.05)
        self.assertIsNone(arms.bootstrap_mean_interval([], 10, 1))

    def test_auroc_counts_ties_as_half(self):
        self.assertEqual(arms.auroc([2.0, 1.0], [1.0, 0.0]), 3.5 / 4)
        self.assertIsNone(arms.auroc([], [1.0]))

    def test_margin_of_equal_infinities_is_zero(self):
        self.assertEqual(arms.margin(float("-inf"), float("-inf")), 0.0)
        self.assertEqual(arms.margin(-2.0, -3.0), 1.0)

    def test_quantile_type7(self):
        self.assertAlmostEqual(arms.quantile_type7(range(21), 0.05), 1.0)
        self.assertAlmostEqual(arms.quantile_type7([1.0, 2.0, 4.0], 0.05), 1.1)

    def test_research_decisions_abstain_below_the_conformal_minimum(self):
        rng = np.random.default_rng(3)
        reference, tests = rng.normal(size=(12, 3)), rng.normal(size=(4, 3))
        result = arms.research_decisions(arms.pca_scores, reference, tests, 10, 19)
        self.assertEqual(result["in_sample"]["status"], "decided")
        self.assertEqual(result["cross_conformal"]["status"], "abstained")
        larger = arms.research_decisions(arms.ledoit_wolf_scores, rng.normal(size=(40, 3)), tests, 10, 19)
        self.assertEqual(len(larger["cross_conformal"]["flags"]), 4)


def summary_with(arm_entry: dict, baseline_entry: dict, cells=("summary-c6-n32",), family="exchangeable"):
    return {family: {cell: {"baseline": baseline_entry, "cross_conformal": arm_entry} for cell in cells}}


DETECTED = {kind: {"rate": 0.95} for kind in cytoscan.ANOMALIES}


class Decision(unittest.TestCase):
    good = {"decided": 50, "replicates": 50, "decision_rate": 1.0, "fpr_mean": 0.05,
            "fpr_ci95": [0.04, 0.06], "flip_normals_mean": 0.06, "detection": DETECTED}
    base = {"decided": 50, "replicates": 50, "decision_rate": 1.0, "fpr_mean": 0.3,
            "fpr_ci95": [0.25, 0.35], "flip_normals_mean": 0.1, "detection": DETECTED}

    def test_eligible_only_on_the_confirmation_profile(self):
        summary = summary_with(self.good, self.base)
        self.assertTrue(nb.decide(summary, "confirmation", True)["cross_conformal"]["eligible"])
        verdict = nb.decide(summary, "development", True)["cross_conformal"]
        self.assertFalse(verdict["eligible"])
        self.assertIn("profile development is not the decision profile", verdict["failed_criteria"])
        self.assertFalse(nb.decide(summary, "confirmation", False)["cross_conformal"]["eligible"])

    def test_a_failed_cell_fails_the_arm(self):
        bad = dict(self.good, fpr_ci95=[0.05, 0.12])
        verdict = nb.decide(summary_with(bad, self.base), "confirmation", True)["cross_conformal"]
        self.assertFalse(verdict["eligible"])
        self.assertIn("exchangeable/summary-c6-n32: fpr_upper95", verdict["failed_criteria"])
        weak = dict(self.good, detection={**DETECTED, "wrong_spill": {"rate": 0.5}})
        verdict = nb.decide(summary_with(weak, self.base), "confirmation", True)["cross_conformal"]
        self.assertIn("exchangeable/summary-c6-n32: detection", verdict["failed_criteria"])

    def test_flip_rate_bound_is_absolute(self):
        # A baseline that flags every file has no flips; rule v2 bounds the
        # arm's flips on normals at 0.10 instead of comparing with it.
        degenerate = dict(self.base, fpr_mean=1.0, flip_normals_mean=0.0)
        self.assertTrue(nb.decide(summary_with(self.good, degenerate), "confirmation", True)
                        ["cross_conformal"]["eligible"])
        flippy = dict(self.good, flip_normals_mean=0.11)
        verdict = nb.decide(summary_with(flippy, self.base), "confirmation", True)["cross_conformal"]
        self.assertIn("exchangeable/summary-c6-n32: novelty_flip_rate", verdict["failed_criteria"])

    def test_small_references_must_abstain(self):
        flagged = dict(self.good)
        summary = summary_with(flagged, self.base, cells=("summary-c6-n16", "summary-c6-n32"))
        verdict = nb.decide(summary, "confirmation", True)["cross_conformal"]
        self.assertIn("exchangeable/summary-c6-n16: abstains_below_reference", verdict["failed_criteria"])

    def test_nothing_evaluated_is_not_eligible(self):
        verdict = nb.decide({}, "confirmation", True)["seed_ensemble_5"]
        self.assertFalse(verdict["eligible"])
        self.assertIn("nothing to stabilize", verdict["failed_criteria"][0])

    def test_stability_claim_needs_halved_flips(self):
        base = dict(self.base, flip_normals_mean=0.10)
        better = dict(self.base, arm="seed_ensemble_5", flip_normals_mean=0.04,
                      paired_fpr_difference_ci95=[-0.01, 0.01])
        summary = {"exchangeable": {"quantiles-c6-n32": {"baseline": base, "seed_ensemble_5": better}}}
        self.assertTrue(nb.decide(summary, "confirmation", True)["seed_ensemble_5"]["eligible"])
        worse = dict(better, flip_normals_mean=0.06)
        summary["exchangeable"]["quantiles-c6-n32"]["seed_ensemble_5"] = worse
        self.assertFalse(nb.decide(summary, "confirmation", True)["seed_ensemble_5"]["eligible"])


class Table(unittest.TestCase):
    def test_refuses_a_report_under_another_rule(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "report.json"
            path.write_text(json.dumps({"decision_rule_sha256": "0" * 64, "status": "completed"}))
            with self.assertRaises(SystemExit) as raised:
                nb.table_command(Namespace(report=[path], output=Path(directory) / "table"))
            self.assertIn("another decision rule", str(raised.exception))


class Generator(unittest.TestCase):
    def test_parity_cohort_bytes_are_unchanged(self):
        pins = json.loads((ROOT / "data/fixtures/parity/sha256.json").read_text())["files"]
        with tempfile.TemporaryDirectory() as directory:
            cytoscan.gen_cytoscan_exact(Path(directory))
            for path in sorted(Path(directory).rglob("*.fcs")):
                key = f"data/fixtures/parity/cytoscan_exact/{path.relative_to(directory)}"
                self.assertEqual(hashlib.sha256(path.read_bytes()).hexdigest(), pins[key], key)

    def test_control_batch_is_the_cohort_arithmetic(self):
        for anomaly in (None, "debris", "wrong_spill"):
            old = cytoscan._file(np.random.default_rng(5), 500, anomaly)
            new = cytoscan._file_batch(np.random.default_rng(5), 500, anomaly, cytoscan.Batch())
            np.testing.assert_array_equal(old[0], new[0])
            np.testing.assert_array_equal(old[1], new[1])

    def test_batches_share_the_latent_draws(self):
        quiet = cytoscan._file_batch(np.random.default_rng(9), 400, None, cytoscan.Batch(noise_sd=0.0))
        gained = cytoscan._file_batch(np.random.default_rng(9), 400, None,
                                      cytoscan.Batch(gain=0.1, noise_sd=0.0))
        inside = np.abs(gained[0]) < cytoscan.PNR - 1
        np.testing.assert_allclose(gained[0][inside], 1.1 * quiet[0][inside], rtol=1e-12)

    def test_files_do_not_depend_on_the_pool_size(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            cytoscan.write_files(root / "a", np.random.SeedSequence(7, spawn_key=(0,)),
                                 [("f0.fcs", None), ("f1.fcs", None)], 300)
            cytoscan.write_files(root / "b", np.random.SeedSequence(7, spawn_key=(0,)),
                                 [("f0.fcs", None)], 300)
            self.assertEqual((root / "a/f0.fcs").read_bytes(), (root / "b/f0.fcs").read_bytes())


if __name__ == "__main__":
    unittest.main()
