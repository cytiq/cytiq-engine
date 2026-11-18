"""Fast deterministic tests of the AF benchmark: the port of the production
estimator, the research arms, the solvers, identifiability and the decision
rule. The port is also checked against the Rust binary by `af_benchmark.py
oracle` (CI), which these tests cannot replace."""
from functools import reduce
from pathlib import Path
import re
import unittest

import numpy as np

import af_benchmark
import af_estimators as est

FIXTURE = Path(__file__).resolve().parents[2] / "data/fixtures/autospectral/rlm_reference.rs"


def rust_constants(path: Path) -> dict:
    """The arrays and scalars of `parity/make_fixtures.R`'s Rust include file."""
    text = path.read_text()
    values = {name: np.array([float(v) for v in body.split(",") if v.strip()])
              for name, body in re.findall(r"const (\w+): \[f64; \d+\] = \[([^\]]*)\]", text)}
    values.update({name: float(v) for name, v in re.findall(r"const (\w+): f64 = ([^;]+);", text)})
    values.update({name: v == "true" for name, v in re.findall(r"const (\w+): bool = (\w+);", text)})
    return values


def two_source_events(seed=3, n=400):
    """Noise-free events of two AF sources peaking on different detectors with
    near-equal means (the runner.rs `two_component_af_with_near_tied_peaks_is_flagged`
    construction)."""
    a = np.array([1.0, 0.3, 0.1, 0.05])
    b = np.array([0.05, 0.2, 1.0, 0.4])
    events = np.arange(n)
    return np.outer(50.0 + (events * 37 % 101), a) + np.outer(49.0 + (events * 53 % 97), b), a, b


class PortTests(unittest.TestCase):
    def test_robust_slope_matches_mass_rlm_at_the_rust_tolerances(self):
        constants = rust_constants(FIXTURE)
        self.assertTrue(constants["RLM_CONVERGED"])
        slope = est.robust_slope(constants["RLM_X"], constants["RLM_Y"])
        self.assertLess(abs(slope - constants["RLM_SLOPE"]), 1e-6 * abs(constants["RLM_SLOPE"]))
        self.assertFalse(constants["RLM_DIVERGENT_CONVERGED"])
        slope = est.robust_slope(constants["RLM_DIVERGENT_X"], constants["RLM_DIVERGENT_Y"])
        self.assertLess(abs(slope - constants["RLM_DIVERGENT_SLOPE"]), 1e-9 * abs(constants["RLM_DIVERGENT_SLOPE"]))

    def test_spectrum_closed_forms_of_spectrum_rs(self):
        x = np.arange(50, dtype=float)
        spectrum = est.spectrum([0.5 * x + 3.0, x, 2.0 * x], 1)
        np.testing.assert_allclose(spectrum, [0.25, 0.5, 1.0], atol=1e-12)
        self.assertIsNone(est.spectrum([np.full(10, 5.0), np.arange(10, dtype=float)], 0))

    def test_sequential_sum_is_a_left_fold_unlike_pairwise_sum(self):
        values = np.array([1.0, 1e100, 1.0, -1e100] * 64 + [0.1] * 1000)
        left = reduce(lambda total, v: total + v, values.tolist(), 0.0)
        self.assertEqual(float(est.sequential_sum(values)), left)
        self.assertNotEqual(float(np.sum(values)), left)
        self.assertEqual(float(est.sequential_sum([])), 0.0)

    def test_first_argmax_follows_which_max(self):
        self.assertEqual(est.first_argmax([1.0, 3.0, 3.0, 2.0]), 1)
        self.assertEqual(est.first_argmax([1.0, 3.0, 3.0, 2.0], 1), 2)
        self.assertEqual(est.first_argmax([-0.0, 0.0]), 0)
        self.assertIsNone(est.first_argmax([5.0], 0))

    def test_quantile_type7_and_median_midpoint(self):
        values = np.array([4.0, 1.0, 3.0, 2.0])
        self.assertEqual(est.quantile_type7(values, 0.5), 2.5)
        self.assertEqual(est.quantile_type7(values, 1.0), 4.0)
        self.assertEqual(est.median_midpoint(values), 2.5)
        self.assertEqual(est.median_midpoint(values[:3]), 3.0)

    def test_scatter_neighbours_take_first_neighbours_first(self):
        negative = np.array([[0.0, 0.0], [10.0, 0.0], [1.0, 0.0], [11.0, 0.0], [5.0, 5.0]])
        matched = est.scatter_neighbours(np.array([[0.2, 0.0], [10.2, 0.0]]), negative)
        np.testing.assert_array_equal(matched, [0, 1, 2, 3, 4])
        np.testing.assert_array_equal(est.evenly_spaced(matched, 2), [0, 2])

    def test_positive_rows_keep_the_brightest_with_ties_in_row_order(self):
        primary = np.array([0.0, 5.0, 9.0, 9.0, 7.0] + [1.0] * 30)
        rows = est.positive_rows(primary, np.arange(primary.size), np.zeros(100))
        np.testing.assert_array_equal(rows, np.sort(np.flatnonzero(primary > 0)))
        with self.assertRaises(ValueError):
            est.positive_rows(primary[:5], np.arange(5), np.zeros(10))


class ArmTests(unittest.TestCase):
    def test_every_single_af_arm_recovers_a_noise_free_single_source(self):
        source = np.array([0.2, 1.0, 0.98, 0.5])
        Y = np.outer(100.0 + (np.arange(400) * 37 % 101), source)
        for name, arm in est.SINGLE_AF_ARMS.items():
            spectra, _ = arm(Y, None)
            self.assertGreaterEqual(est.matched_cosines(spectra, source[None, :]).min(), 1 - 1e-12, name)

    def test_near_tied_two_source_af_moves_with_the_peak_detector(self):
        Y, _, _ = two_source_events()
        peak, _ = est.arm_peak_argmax(Y)
        runner, _ = est.arm_runner_up(Y)
        self.assertLess(float(est.cosine_matrix(peak, runner)[0, 0]), 0.9999)

    def test_scatter_strata_split_by_ssc_quantiles(self):
        Y, a, b = two_source_events()
        scatter = np.column_stack([np.zeros(400), np.arange(400, dtype=float)])
        spectra, info = est.arm_scatter_strata(2)(Y, scatter)
        self.assertEqual(info["events_per_component"], [200, 200])
        self.assertEqual(spectra.shape, (2, 4))

    def test_spherical_kmeans_is_deterministic_per_seed(self):
        rng = np.random.default_rng(0)
        units = rng.normal(size=(300, 4))
        units /= np.linalg.norm(units, axis=1, keepdims=True)
        first = est.spherical_kmeans(units, 3, seed=5)
        second = est.spherical_kmeans(units, 3, seed=5)
        np.testing.assert_array_equal(first[1], second[1])
        np.testing.assert_array_equal(first[0], second[0])


class SolverTests(unittest.TestCase):
    def setUp(self):
        self.fluorophores = np.array([[1.0, 0.1], [0.3, 0.2], [0.05, 1.0], [0.0, 0.5], [0.1, 0.05], [0.0, 0.3]])
        self.afs = np.array([[0.6, 1.0, 0.3, 0.1, 0.8, 0.05], [0.1, 0.2, 0.4, 1.0, 0.1, 0.9]])

    def test_joint_ols_recovers_known_fluorophores_with_both_afs(self):
        truth = np.array([[100.0, 20.0, 50.0, 7.0], [3.0, 400.0, 9.0, 60.0]])
        reference = np.column_stack([self.fluorophores, self.afs.T])
        Y = truth @ reference.T
        abundances, residuals, _ = est.unmix_joint(Y, self.fluorophores, self.afs)
        np.testing.assert_allclose(abundances, truth, atol=1e-9)
        self.assertLess(residuals.max(), 1e-9)

    def test_min_residual_assignment_picks_the_true_source(self):
        rows = []
        for j, af in enumerate(self.afs):
            rows.append(np.array([30.0, 5.0]) @ self.fluorophores.T + 200.0 * af)
        abundances, residuals, choice = est.unmix_min_residual(np.vstack(rows), self.fluorophores, self.afs)
        np.testing.assert_array_equal(choice, [0, 1])
        np.testing.assert_allclose(abundances[:, :2], [[30.0, 5.0], [30.0, 5.0]], atol=1e-9)
        np.testing.assert_array_equal(est.assign_upstream_score(np.vstack(rows), self.fluorophores, self.afs), [0, 1])

    def test_identifiability_rejects_duplicate_and_fluorophore_like_afs(self):
        condition = est.condition_number(np.column_stack([self.fluorophores, self.afs[:1].T]))
        ok = est.identifiability(self.afs, [500, 500], self.fluorophores, condition * 10, 0.999)
        self.assertTrue(ok["checks"]["pairwise_af_cosine"])
        duplicate = est.identifiability(np.vstack([self.afs[0], self.afs[0] * 1.001]), [500, 500], self.fluorophores, condition, 0.999)
        self.assertFalse(duplicate["identifiable"])
        self.assertFalse(duplicate["checks"]["pairwise_af_cosine"])
        collinear = est.identifiability(np.vstack([self.afs[0], self.fluorophores[:, 0]]), [500, 500], self.fluorophores, condition, 0.999)
        self.assertFalse(collinear["checks"]["af_fluorophore_cosine"])
        few = est.identifiability(self.afs, [500, 10], self.fluorophores, condition * 10, 0.999)
        self.assertFalse(few["checks"]["events_per_component"])

    def test_auc_and_principal_angle(self):
        self.assertEqual(est.auc(np.array([2.0, 3.0]), np.array([0.0, 1.0])), 1.0)
        self.assertEqual(est.auc(np.array([1.0]), np.array([1.0])), 0.5)
        self.assertAlmostEqual(est.largest_principal_angle(np.array([[1.0, 0.0, 0.0]]), np.array([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0]])), 0.0)
        self.assertAlmostEqual(est.largest_principal_angle(np.array([[0.0, 0.0, 1.0]]), np.array([[1.0, 0.0, 0.0]])), np.pi / 2)


class DecisionTests(unittest.TestCase):
    @staticmethod
    def dataset(label, kind, ratio):
        arm = lambda: {"status": "ok", "stability": {"split_half_min_component_cosine": 0.9999},  # noqa: E731
                       "conditioning": {"condition_ratio_to_production": 1.0, "max_af_fluorophore_cosine": 0.5}}
        held = lambda r: {"status": "ok", "median_robust_sd_ratio": r, "max_p99_ratio": 1.0}  # noqa: E731
        return {"label": label, "kind": kind, "oracle": {"status": "passed"},
                "arms": {"peak_argmax": arm(), "mean_ratio": arm()},
                "held_out_unstained": {"variants": {"peak_argmax": held(1.0), "mean_ratio": held(ratio)}}}

    def test_one_real_dataset_is_never_enough(self):
        decision = af_benchmark.decide([self.dataset("aurora", "real", 0.5)])
        self.assertFalse(decision["mean_ratio"]["eligible"])
        self.assertIn("1 independent real dataset(s) < 2", decision["mean_ratio"]["failed_criteria"])

    def test_two_real_wins_and_no_synthetic_loss_are_eligible(self):
        datasets = [self.dataset("a", "real", 0.9), self.dataset("b", "real", 0.9), self.dataset("s1", "single_af", 1.0)]
        self.assertTrue(af_benchmark.decide(datasets)["mean_ratio"]["eligible"])
        datasets.append(self.dataset("s2", "two_af_near_tie", 0.97))
        decision = af_benchmark.decide(datasets)["mean_ratio"]
        self.assertFalse(decision["eligible"])
        self.assertEqual(decision["failed_criteria"], ["s2: held_out_median_robust_sd_ratio"])

    def test_the_decision_rule_hash_is_pinned(self):
        # Changing DECISION_RULE after results exist is a new rule: update this
        # hash, the README and every report that cites it deliberately.
        self.assertEqual(af_benchmark.DECISION_RULE_SHA256,
                         "aa5533e0114922bec592572f7cd80992b9f3454a313c5ba96ce297fd497e6eba")


if __name__ == "__main__":
    unittest.main()
