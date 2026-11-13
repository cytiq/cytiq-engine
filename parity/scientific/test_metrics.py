import unittest
import numpy as np
from metrics import bootstrap_interval, coefficient_perturbations, empirical_spreading, retention, spectral_metrics, wilson_interval, unmixing_spreading


class ScientificMetricsTests(unittest.TestCase):
    def test_unmixing_spread_uses_inverse_operator_and_component_orientation(self):
        matrix=np.eye(2)
        result=unmixing_spreading(matrix,matrix,events=4000)
        np.testing.assert_allclose(result["predicted_variance_excess_per_signal"],np.eye(2)*1.5)
        np.testing.assert_allclose(result["empirical_variance_excess_per_signal"],np.eye(2)*1.5,atol=.12)
    def test_coefficient_perturbation_first_order_error_is_bounded(self):
        rng = np.random.default_rng(7)
        spillover = np.eye(3) + rng.uniform(0, .3, (3, 3)) * (1 - np.eye(3))
        latent = rng.lognormal(4, 1, (2000, 3))
        result = coefficient_perturbations(spillover, latent, deltas=(1e-3, .1))
        self.assertEqual(len(result), 6)
        for cell in result.values():
            small, large = cell["0.001"], cell["0.1"]
            for entry in (small, large):
                self.assertLessEqual(entry["first_order_relative_error"], entry["bound"] + 1e-12)
            # Error grows linearly to first order: a 100x larger delta.
            self.assertAlmostEqual(large["rmse"] / small["rmse"], 100, delta=100 * large["bound"] * 3)
        # A negligible coefficient error leaves a negligible compensation error.
        exact = coefficient_perturbations(spillover, latent, deltas=(1e-12,))
        for cell in exact.values():
            self.assertLess(cell["1e-12"]["rmse"], 1e-6)

    def test_unit_noise_gain_matches_exact_inverse(self):
        matrix = np.array([[1., .5], [0., 1.]])
        result = spectral_metrics(matrix)
        np.testing.assert_allclose(result["unit_noise_covariance"], [[1.25, -.5], [-.5, 1.]])

    def test_spreading_orientation_and_units(self):
        positive = np.array([[-2., -4.], [0., 0.], [2., 4.]])
        negative = np.zeros((3, 2))
        spread = np.array(empirical_spreading(positive, negative, 4)["spread"])
        np.testing.assert_allclose(spread, [1., 2.])
        np.testing.assert_allclose(empirical_spreading(positive*3, negative, 4)["spread"], spread*3)
        np.testing.assert_allclose(empirical_spreading(positive, negative, 16)["spread"], spread/2)

    def test_rare_loss_and_technical_removal_have_separate_denominators(self):
        labels = ["common"]*999 + ["rare_dim"]
        keep = np.arange(1000) < 999
        report = retention(labels, [("qc", keep)], np.ones(1000, dtype=bool))[0]
        self.assertEqual(report["newly_removed"], 1)
        self.assertEqual(report["populations"]["rare_dim"]["good_retention"], 0.)
        self.assertEqual(report["populations"]["rare_dim"]["relative_frequency_error"], -1.)

    def test_intervals_are_deterministic_and_do_not_claim_zero_uncertainty(self):
        self.assertEqual(bootstrap_interval([1., 2., 3.], replicates=64), bootstrap_interval([1., 2., 3.], replicates=64))
        self.assertGreater(wilson_interval(0, 8)[1], .3)
        with self.assertRaises(ValueError):
            bootstrap_interval([1.], replicates=1000)


if __name__ == "__main__":
    unittest.main()
