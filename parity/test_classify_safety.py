import tempfile
import unittest
from pathlib import Path

import pyarrow as pa
import pyarrow.parquet as pq

from classify_safety import evaluate


class EvaluationTests(unittest.TestCase):
    def run_evaluation(self, probabilities, labels, *, multiclass=False, cutoff=0.75, ood=False, overlap=False, version=2):
        k = len(probabilities[0])
        roles = ["train", "calibration", "selection"] + ["test"]*len(labels)
        groups = [f"{i:064x}" for i in [1, 2, 3]] + [f"{4:064x}"]*len(labels)
        if overlap:
            groups[-1] = groups[0]
        p = [probabilities[0]]*3 + probabilities
        y = [0]*3 + labels
        if ood:
            roles += ["ood_test"]; groups += [f"{5:064x}"]; p += [[0.99]+[0.01/(k-1)]*(k-1)]; y += [None]
        table = pa.table({"split": roles, "group_id": groups,
                          "probabilities": pa.array(p, type=pa.list_(pa.float32(), k)),
                          "label_index": pa.array(y, type=pa.int32()),
                          "is_ood": [role == "ood_test" for role in roles],
                          "ood_score": pa.array([1.0 if role == "ood_test" else 0.0 for role in roles],type=pa.float32()),
                          "rare": [False]*3 + [True]*len(labels) + ([False] if ood else [])})
        classes = [str(i) for i in range(k)]
        candidate = {"classes": classes, "model_sha256": "model", "min_confidence": cutoff,
                     "ood": {"threshold": 0.5} if ood else None}
        bundle = {"schema_version":version,"classes": classes, "model_sha256": "model", "threshold": 0.5,
                  "task_kind": "multiclass" if multiclass else "binary_phenotype"}
        limits = {"max_brier": 1.0, "max_ece": 1.0, "max_risk": 1.0, "min_coverage": 0.0,
                  "max_ood_false_accept_rate": 0.1 if ood else None}
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp)/"events.parquet"; pq.write_table(table,path)
            return evaluate(path,candidate,bundle,limits)

    def test_binary_metrics_and_risk_coverage_keep_complete_ties(self):
        validation, report = self.run_evaluation([[0.8,0.2],[0.2,0.8],[0.2,0.8],[0.5,0.5]],[0,1,0,1])
        metrics = validation["metrics"]
        self.assertAlmostEqual(metrics["brier"],0.2425,places=7)
        self.assertAlmostEqual(metrics["ece"],0.325,places=7)
        self.assertAlmostEqual(metrics["risk"],1/3)
        self.assertEqual(metrics["coverage"],0.75)
        self.assertEqual(len(report["risk_coverage"]),2)
        self.assertEqual(report["risk_coverage"][0]["coverage"],0.75)
        self.assertEqual(report["rare"]["events"],4)

    def test_multiclass_brier_and_high_confidence_ood_are_separate(self):
        validation, report = self.run_evaluation([[0.8,0.1,0.1],[0.1,0.8,0.1]],[0,2],multiclass=True,ood=True)
        self.assertAlmostEqual(validation["metrics"]["brier"],0.76,places=7)
        self.assertAlmostEqual(validation["metrics"]["ece"],0.3,places=7)
        self.assertEqual(validation["metrics"]["ood_false_accept_rate"],0.0)
        self.assertEqual(report["ood_events"],1)

    def test_no_accepted_events_has_no_measured_risk(self):
        validation, _ = self.run_evaluation([[0.8,0.2]],[0],cutoff=1.0)
        self.assertIsNone(validation["metrics"]["risk"])
        self.assertEqual(validation["metrics"]["coverage"],0.0)

    def test_v1_confidence_uses_the_public_complement_probability(self):
        validation, _ = self.run_evaluation([[0.599999,0.4]],[0],cutoff=0.6,version=1)
        self.assertEqual(validation["metrics"]["coverage"],1.0)

    def test_subject_leakage_and_nonfinite_probabilities_are_rejected(self):
        with self.assertRaisesRegex(ValueError,"overlap"):
            self.run_evaluation([[0.8,0.2]],[0],overlap=True)
        with self.assertRaisesRegex(ValueError,"finite"):
            self.run_evaluation([[float("nan"),0.2]],[0])


if __name__ == "__main__":
    unittest.main()
