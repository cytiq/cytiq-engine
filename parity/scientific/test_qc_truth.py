"""The artifact-truth oracle, the policy protocol and its leakage guard."""
import json
import tempfile
import unittest
from pathlib import Path

import numpy as np

from metrics import (artifact_removal_oracle, base_name, event_states, membership_retention_oracle,
                     tri_state)
import qc_truth

YES, NO = True, False


def runs(spec):
    labels, values = [], []
    for label, value, count in spec:
        labels += [label] * count
        values += [value] * count
    return labels, values


def oracle(filters, truth, keep=None, ids=None):
    events = len(next(iter(truth.values())))
    codes = {name: tri_state(values) for name, values in truth.items()}
    if keep is None:
        keep = [all(values[row] is not False for _, values in filters) for row in range(events)]
    return artifact_removal_oracle(range(events) if ids is None else ids, keep, filters, codes, events)


def triple(estimate):
    return estimate["value"], estimate["lower"], estimate["upper"]


class ArtifactOracle(unittest.TestCase):
    def asymmetric(self):
        # Artifacts 5/2/1 (removed/passed/not assessed), clean 3/11/2,
        # unlabelled 4 removed and 6 passed: the Rust unit test's counts.
        return runs([(YES, NO, 5), (YES, YES, 2), (YES, None, 1), (NO, NO, 3), (NO, YES, 11),
                     (NO, None, 2), (None, NO, 4), (None, YES, 6)])

    def test_column_metrics_match_hand_counts(self):
        labels, values = self.asymmetric()
        result = oracle([("non_debris", values)], {"debris": labels})
        column = result["filters"][0]
        self.assertEqual(column["assessed"], 31)
        self.assertEqual(triple(column["clean_loss"]), (3 / 16, 3 / 22, 7 / 20))
        self.assertEqual(triple(column["removed_artifact_share"]), (5 / 8, 5 / 12, 9 / 12))
        target = column["target"]
        self.assertEqual(triple(target["sensitivity"]), (5 / 8, 5 / 14, 9 / 12))
        self.assertEqual(target["assessed_sensitivity"], 5 / 7)
        self.assertEqual(triple(target["specificity"]), (13 / 16, 13 / 20, 19 / 22))
        self.assertEqual(triple(target["precision"]), (5 / 8, 5 / 12, 9 / 12))
        combined = result["combined"]
        self.assertEqual(combined["kept"], 22)
        self.assertEqual(triple(combined["artifact_recall"]), (5 / 8, 5 / 14, 9 / 12))
        self.assertEqual(triple(combined["clean_retention"]), (13 / 16, 13 / 20, 19 / 22))
        self.assertEqual(result["events"], {"raw": 34, "clean": 16, "artifact": 8, "undetermined": 10,
                                            "clean_of": ["debris"]})

    def test_targets_without_labels_carry_no_number(self):
        result = oracle([("singlets", [NO, YES]), ("live", [NO, YES]), ("2.margin_pass", [None, None]),
                         ("margin_lower_pass", [YES, NO])],
                        {"debris": [YES, NO], "dead": [None, None], "saturated": [YES, NO]})
        singlets, live, margin, lower = result["filters"]
        self.assertEqual(singlets["target"], {"class": "doublet", "status": "not_assessed",
                                              "reason": "the artifact truth does not label `doublet`"})
        self.assertEqual(live["target"]["reason"], "no event is labelled for `dead`")
        self.assertEqual(margin["target"]["reason"], "`2.margin_pass` assessed no event")
        self.assertNotIn("target", lower)

    def test_zero_denominators_are_null(self):
        result = oracle([("singlets", [YES] * 4)], {"doublet": [NO] * 4})
        target = result["filters"][0]["target"]
        self.assertEqual(triple(target["sensitivity"]), (None, None, None))
        self.assertNotIn("assessed_sensitivity", target)
        self.assertEqual(target["specificity"]["value"], 1.0)

    def test_rows_are_aligned_by_source_event_id(self):
        result = oracle([("non_debris", [YES, NO, YES])], {"debris": [YES, NO, NO]},
                        keep=[True, False, True], ids=[2, 0, 1])
        self.assertEqual(result["combined"]["artifact_recall"]["value"], 1.0)
        self.assertEqual(result["combined"]["clean_retention"]["value"], 1.0)
        with self.assertRaises(ValueError):
            oracle([("non_debris", [YES, YES])], {"debris": [YES, NO]}, keep=[True, True], ids=[0, 0])

    def test_event_states_take_any_artifact(self):
        states = event_states({"doublet": tri_state([YES, NO, NO, None]), "dead": tri_state([None, NO, None, NO])})
        self.assertEqual(states.tolist(), [1, 0, 2, 2])

    def test_combine_prefixes(self):
        self.assertEqual([base_name(c) for c in ("2.2.live", "3x.live", "live")], ["live", "3x.live", "live"])


class RetentionOracle(unittest.TestCase):
    def test_frequency_error_against_clean_events(self):
        t = np.array([1, 1, 0, 0, 1, 1, 1, 1], dtype=bool)
        states = event_states({"debris": tri_state([NO] * 4 + [YES] * 4)})
        keep = np.arange(8) < 4
        row = membership_retention_oracle([("t", t)], [("filter", keep)], states)[0]["populations"][0]
        self.assertEqual((row["original_frequency"], row["retained_frequency"]), (0.75, 0.5))
        self.assertEqual((row["clean_frequency"], row["frequency_error_vs_clean"]), (0.5, 0.0))
        self.assertEqual((row["technical_removed"], row["good_retention"]), (4, 1.0))


class Policy(unittest.TestCase):
    def test_protocol_is_pinned(self):
        self.assertEqual(qc_truth.protocol_sha256(),
                         "438b7fac6d70c7af86b589a5f941708d17d93569759f97fe23769533949880c4",
                         "the policy protocol is a reviewed change: update this pin with it")

    def dataset(self, name, acquisitions, real=True, truth="independent"):
        return {"name": name, "real": real, "truth_classes": {"debris": truth}, "acquisitions": acquisitions}

    def test_leakage_guard_rejects_shared_acquisitions(self):
        qc_truth.disjoint_acquisitions(["a", "b"], ["c"])
        with self.assertRaisesRegex(ValueError, "share acquisitions"):
            qc_truth.disjoint_acquisitions(["a", "b"], ["b"])
        with self.assertRaisesRegex(ValueError, "share acquisitions"):
            qc_truth.policy_design([self.dataset("x", ["a"]), self.dataset("y", ["b"])],
                                   [self.dataset("z", ["a"])])

    def test_design_needs_real_eligible_truth(self):
        good = [self.dataset("x", ["a"]), self.dataset("y", ["b"], truth="expert_reference")]
        qc_truth.policy_design(good, [self.dataset("z", ["c"])])
        cases = [
            ([self.dataset("x", ["a"])], [self.dataset("z", ["c"])], "two"),
            (good, [], "held-out"),
            (good, [self.dataset("z", ["c"], real=False)], "synthetic"),
            (good, [self.dataset("z", ["c"], truth="synthetic")], "cannot derive"),
            (good, [self.dataset("z", ["c"], truth="exploratory")], "cannot derive"),
        ]
        for derivation, evaluation, message in cases:
            with self.assertRaisesRegex(ValueError, message):
                qc_truth.policy_design(derivation, evaluation)

    def test_no_registered_real_data_is_not_assessed(self):
        self.assertEqual(qc_truth.policy_status()["status"], "not_assessed")
        self.assertEqual(qc_truth.REAL_DATA["status"], "not_assessed")
        self.assertTrue(all(v == "not_assessed" for v in qc_truth.REAL_DATA["domains"].values()))


class Report(unittest.TestCase):
    def test_reference_text_round_trips(self):
        reference = {"schema_version": 1, "baseline_commit": "abc",
                     "cases": {"b": {"2": {"x": [1, {"y": None}]}, "1": {"x": []}}, "a": {"3": {}}}}
        self.assertEqual(json.loads(qc_truth.reference_text(reference)), reference)

    def test_frozen_reference_is_parseable_and_counts_only(self):
        reference = json.loads(qc_truth.REFERENCE.read_text())
        self.assertEqual(set(reference["cases"]), set(qc_truth.SCENARIOS))

        def numbers(value):
            if isinstance(value, dict):
                return [n for v in value.values() for n in numbers(v)]
            if isinstance(value, list):
                return [n for v in value for n in numbers(v)]
            return [value] if isinstance(value, (int, float)) and not isinstance(value, bool) else []
        values = numbers(reference["cases"])
        self.assertTrue(values and all(isinstance(v, int) for v in values), "counts only, no ratios")

    def test_comparison_is_type_strict(self):
        self.assertIsNone(qc_truth.first_difference({"a": [1, 0.5]}, {"a": [1, 0.5]}))
        self.assertIn("$.a[0]", qc_truth.first_difference({"a": [1]}, {"a": [1.0]}))
        self.assertIn("$.b", qc_truth.first_difference({"a": 1}, {"a": 1, "b": None}))

    def test_table_prints_no_number_for_not_assessed(self):
        empty = {"n": 0}
        filters = {column: {"status": ["not_assessed"], "clean_loss": {"n": 1, "min": 0.0, "median": 0.0, "max": 0.0},
                            "sensitivity": empty, "specificity": empty, "precision": empty}
                   for column in qc_truth.DOMAIN_COLUMNS.values()}
        summary = {"baseline": {"domain": "all", "qc_exit_codes": [0], "filters": filters,
                                "combined": {k: empty for k in ("artifact_recall", "clean_retention",
                                                                "removed_artifact_share", "total_retention")},
                                "populations": {"rare": {"good_retention": empty,
                                                         "relative_frequency_error_vs_clean": empty}}}}
        report = {"status": "completed", "validation": "synthetic_only", "seeds": [1],
                  "truth_classes": qc_truth.TRUTH_CLASS, "real_data": qc_truth.REAL_DATA,
                  "policy_derivation": qc_truth.policy_status(), "summary": summary}
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "report.json"
            path.write_text(json.dumps(report))
            qc_truth.table(type("Args", (), {"report": path, "output": Path(directory)}))
            lines = (Path(directory) / "table.md").read_text().splitlines()
        rows = [line for line in lines if line.startswith("| baseline |")]
        self.assertTrue(rows)
        for row in rows:
            cells = row.split("|")[2:-1]
            self.assertTrue(all(c.strip() in ("not assessed", "[0]") for c in cells), row)


if __name__ == "__main__":
    unittest.main()
