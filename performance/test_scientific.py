import unittest
from scientific import equal_json


class ScientificComparisonTests(unittest.TestCase):
    def test_only_documented_additive_diagnostics_are_allowed(self):
        before={"samples":[{"rms_residual":1.0}]}
        after={"samples":[{"rms_residual":1.0,"detector_residual_rms":[1.0]}],"reference_diagnostics":{}}
        equal_json(before,after,diagnostics=True)
        with self.assertRaises(ValueError): equal_json(before,after)
        after["samples"][0]["rms_residual"]=1.000001
        with self.assertRaises(ValueError): equal_json(before,after,diagnostics=True)

    def test_af_peak_sensitivity_is_the_only_new_reference_diagnostic(self):
        before={"reference_diagnostics":{"singular_values":[1.0]}}
        after={"reference_diagnostics":{"singular_values":[1.0],"af_peak_sensitivity":{"stable":True}}}
        equal_json(before,after,diagnostics=True)
        after["reference_diagnostics"]["other"]=1
        with self.assertRaises(ValueError): equal_json(before,after,diagnostics=True)
        del after["reference_diagnostics"]["other"]
        after["reference_diagnostics"]["singular_values"]=[1.1]
        with self.assertRaises(ValueError): equal_json(before,after,diagnostics=True)

    def test_model_and_mask_changes_are_not_diagnostics(self):
        with self.assertRaises(ValueError): equal_json({"weights":[1.,2.]},{"weights":[1.,2.1]})
        with self.assertRaises(ValueError): equal_json({"keep":[True]},{"keep":[False]})


if __name__ == "__main__": unittest.main()
