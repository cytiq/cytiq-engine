"""Closed-form checks of the repeat-control study's truth algebra and intervals."""
import unittest

import numpy as np
from scipy import stats

import repeat_controls as rc
from generate import AUTOSPILL_SPILLOVER


class TruthTests(unittest.TestCase):
    def test_gain_normalised_rows_keep_one_on_their_own_detector(self):
        gains = np.array([1.1, 0.9, 1.05, 1.0, 0.8, 1.2])
        truth = rc.gain_normalised(AUTOSPILL_SPILLOVER, gains)
        np.testing.assert_allclose(np.diag(truth), 1.0)
        # A detector read 10% hotter scales its column by the gain ratio.
        self.assertAlmostEqual(truth[1, 2], AUTOSPILL_SPILLOVER[1, 2] * gains[2] / gains[1])
        # Noise-free events through the gains give exactly that row.
        latent = np.zeros(6)
        latent[1] = 1000.0
        observed = latent @ AUTOSPILL_SPILLOVER * gains
        np.testing.assert_allclose(observed / observed[1], truth[1])

    def test_set_truth_is_the_floored_normal_mean_of_tandem_cells(self):
        params = rc.parameters("tandem")
        truth = rc.set_truth(np.ones(6), params)
        rng = np.random.default_rng(0)
        for dye, donor in rc.TANDEMS:
            draws = np.maximum(AUTOSPILL_SPILLOVER[dye, donor] + rng.normal(0, params["tandem_sd"], 200_000), 0)
            self.assertAlmostEqual(truth[dye, donor], draws.mean(), delta=4 * draws.std() / np.sqrt(draws.size))
        untouched = np.ones_like(truth, dtype=bool)
        for dye, donor in rc.TANDEMS:
            untouched[dye, donor] = False
        np.testing.assert_array_equal(truth[untouched], AUTOSPILL_SPILLOVER[untouched])

    def test_null_scenario_draws_nothing(self):
        params = rc.parameters("null")
        np.testing.assert_array_equal(rc.tandem_row(np.random.default_rng(1), params), AUTOSPILL_SPILLOVER)


class IntervalTests(unittest.TestCase):
    def test_t_and_prediction_intervals_match_scipy(self):
        values = np.array([[1.0], [2.0], [4.0], [7.0]])
        low, high = rc.t_interval(values)
        expected = stats.t.interval(0.95, 3, loc=values.mean(), scale=values.std(ddof=1) / 2)
        np.testing.assert_allclose([low[0], high[0]], expected)
        low, high = rc.prediction_interval(values)
        half = stats.t.ppf(0.975, 3) * values.std(ddof=1) * np.sqrt(1 + 1 / 4)
        np.testing.assert_allclose([low[0], high[0]], [values.mean() - half, values.mean() + half])

    def test_coverage_counts_masked_cells_with_a_wilson_interval(self):
        truth = np.array([[0.5, 2.0], [0.5, 0.5]])
        mask = np.array([[False, True], [True, False]])
        result = rc.coverage(np.zeros((2, 2)), np.ones((2, 2)), truth, mask)
        self.assertEqual((result["hits"], result["intervals"]), (1, 2))
        self.assertEqual(result["fraction"], 0.5)
        self.assertLess(result["wilson95"][0], 0.5)
        self.assertGreater(result["wilson95"][1], 0.5)

    def test_population_metrics_on_toy_gates(self):
        values = np.zeros((6, 6))
        values[0, 2] = 5.0  # a double negative above the PE gate
        values[2, 2] = 5.0  # a weak PE event called positive
        values[4, 1] = 9.0  # a FITC single...
        values[4, 2] = 5.0  # ...called PE positive
        labels = np.array([0, 0, 2, 2, 1, 3])
        latent = np.zeros((6, 6))
        latent[4, 1] = 9.0
        metrics = rc.population_metrics(values, labels, np.full(6, 1.0), latent)
        self.assertEqual(metrics["dn_false_positive_rate"][2], 0.5)
        self.assertEqual(metrics["weak_pe_positive_fraction"], 0.5)
        self.assertEqual(metrics["fitc_single_pe_false_positive_rate"], 1.0)
        self.assertEqual(metrics["coexpressing_pe_positive_fraction"], 0.0)


if __name__ == "__main__":
    unittest.main()
