#!/usr/bin/env python3
"""Evaluate a fixed selective classifier; never fit or select thresholds on test data.

Input Parquet: split, group_id (hashed subject ID), probabilities (list<float32>),
label_index (nullable int), is_ood (bool), ood_score (nullable float32), optional rare.
The independently prepared cytiq-ml split manifest owns these assignments/labels.
"""
import argparse
import hashlib
import json
from pathlib import Path

import numpy as np
import pyarrow as pa
import pyarrow.parquet as pq

ROLES = ("train", "calibration", "selection", "test", "ood_test")
ECE_BINS = 15
MAX_EVENTS = 1_000_000
MAX_GROUPS = 10_000


def file_digest(path):
    hasher = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024*1024), b""):
            hasher.update(chunk)
    return hasher.hexdigest()


def digest(value):
    content = json.dumps(value, sort_keys=True, indent=2, ensure_ascii=False, allow_nan=False) + "\n"
    return hashlib.sha256(content.encode()).hexdigest()


class Metrics:
    def __init__(self):
        self.events = self.accepted = self.errors = 0
        self.brier = 0.0
        self.bin_count = np.zeros(ECE_BINS, dtype=np.int64)
        self.bin_confidence = np.zeros(ECE_BINS)
        self.bin_correct = np.zeros(ECE_BINS)

    def add(self, brier, confidence, error, accepted, calibration=None, outcome=None):
        self.events += len(confidence)
        self.accepted += int(accepted.sum())
        self.errors += int(error[accepted].sum())
        self.brier += float(brier.sum())
        calibration = confidence if calibration is None else calibration
        outcome = ~error if outcome is None else outcome
        bins = np.minimum((calibration * ECE_BINS).astype(int), ECE_BINS - 1)
        self.bin_count += np.bincount(bins, minlength=ECE_BINS)
        self.bin_confidence += np.bincount(bins, weights=calibration, minlength=ECE_BINS)
        self.bin_correct += np.bincount(bins, weights=outcome, minlength=ECE_BINS)

    def report(self):
        return {"events": self.events, "brier": self.brier / self.events if self.events else None,
                "ece": float(np.abs(self.bin_confidence-self.bin_correct).sum()/self.events) if self.events else None,
                "risk": self.errors/self.accepted if self.accepted else None,
                "coverage": self.accepted/self.events if self.events else None}


def risk_coverage(confidence, errors, total):
    """Include whole equal-score groups; no arbitrary within-tie ordering."""
    if not len(confidence):
        return []
    order = np.argsort(-confidence, kind="stable")
    confidence, errors = confidence[order], errors[order]
    ends = np.r_[np.flatnonzero(confidence[:-1] != confidence[1:]), len(confidence)-1]
    cumulative = np.cumsum(errors)
    # The curve is sampled for artifact size only, after constructing complete ties.
    if len(ends) > 200:
        ends = ends[np.unique(np.linspace(0, len(ends)-1, 200, dtype=int))]
    return [{"threshold": float(confidence[i]), "coverage": float((i+1)/total),
             "risk": float(cumulative[i]/(i+1))} for i in ends]


def evaluate(path, candidate, bundle, limits):
    classes = candidate["classes"]
    if classes != bundle["classes"] or candidate["model_sha256"] != bundle["model_sha256"]:
        raise ValueError("Policy/bundle class or model mismatch")
    if not 2 <= len(classes) <= 256:
        raise ValueError("Expected 2..256 classes")
    parquet = pq.ParquetFile(path)
    probability_type = parquet.schema_arrow.field("probabilities").type
    if not pa.types.is_fixed_size_list(probability_type) or probability_type.list_size != len(classes) or probability_type.value_type != pa.float32():
        raise ValueError("Probabilities must be fixed_size_list<float32>[K] to bound decoded memory")
    if any(parquet.metadata.row_group(i).total_byte_size > 256*1024*1024 for i in range(parquet.num_row_groups)):
        raise ValueError("Evaluation row group exceeds the 256 MiB decoded memory budget")
    if not 0 < parquet.metadata.num_rows <= MAX_EVENTS:
        raise ValueError(f"Evaluation must contain 1..{MAX_EVENTS} events; prepare a bounded held-out cohort")
    groups = {role: set() for role in ROLES}
    owner = {}
    known, rare = Metrics(), Metrics()
    by_group = {}
    scores, errors = [], []
    ood_events = ood_accepted = 0
    cutoff = candidate.get("min_confidence")
    if cutoff is not None and (not np.isfinite(cutoff) or not 0 <= cutoff <= 1):
        raise ValueError("Invalid confidence cutoff")
    ood = candidate.get("ood")
    if cutoff is None and ood is None:
        raise ValueError("Empty decision policy")
    if ood is not None and not np.isfinite(ood["threshold"]):
        raise ValueError("Invalid OOD threshold")
    columns = ["split", "group_id", "probabilities", "label_index", "is_ood", "ood_score"]
    if "rare" in parquet.schema_arrow.names:
        columns.append("rare")
    for batch in parquet.iter_batches(batch_size=min(65536, 1_000_000//len(classes)), columns=columns):
        rows = batch.to_pydict()
        count = batch.num_rows
        probabilities = np.asarray(rows["probabilities"], dtype=np.float32)
        if probabilities.shape != (count, len(classes)) or not np.isfinite(probabilities).all() or (
                (probabilities < 0) | (probabilities > 1)).any() or (np.abs(probabilities.sum(axis=1)-1) > 1e-5).any():
            raise ValueError("Expected finite normalized float32 probabilities")
        confidence = probabilities.max(axis=1)
        if bundle.get("schema_version") == 1:
            probabilities[:,0] = np.float32(1.0)-probabilities[:,1]
            confidence = probabilities.max(axis=1)
        predictions = probabilities.argmax(axis=1)
        if bundle.get("task_kind") != "multiclass":
            if len(classes) != 2:
                raise ValueError("Binary task needs two classes")
            predictions = (probabilities[:, 1] >= np.float32(bundle.get("threshold",0.5))).astype(int)
        ood_pass = np.ones(count, dtype=bool)
        if ood is not None:
            values = np.asarray(rows["ood_score"], dtype=np.float32)
            if not np.isfinite(values).all():
                raise ValueError("Missing or nonfinite OOD score")
            ood_pass = values < np.float32(ood["threshold"])
        accepted = ood_pass & (confidence >= np.float32(cutoff) if cutoff is not None else True)
        for i, (role, group, is_ood) in enumerate(zip(rows["split"], rows["group_id"], rows["is_ood"])):
            if role not in groups or not isinstance(group, str) or len(group) != 64 or any(c not in "0123456789abcdef" for c in group):
                raise ValueError("Invalid split/group identity")
            if type(is_ood) is not bool or is_ood != (role == "ood_test"):
                raise ValueError("OOD labels and splits disagree")
            if owner.setdefault(group, role) != role:
                raise ValueError("Subject/group overlap between evaluation splits")
            groups[role].add(group)
            if len(owner) > MAX_GROUPS:
                raise ValueError("Evaluation group memory budget exceeded")
            if is_ood and rows["label_index"][i] is not None:
                raise ValueError("OOD events must not be assigned a known class label")
        selected = np.array([role == "test" for role in rows["split"]])
        if selected.any():
            labels = [label for label, selected in zip(rows["label_index"], selected) if selected]
            if any(type(label) is not int or not 0 <= label < len(classes) for label in labels):
                raise ValueError("Known test events require valid independent labels")
            labels = np.asarray(labels, dtype=int)
            p = probabilities[selected].astype(np.float64)
            if bundle.get("task_kind") == "multiclass":
                one_hot = np.eye(len(classes))[labels]
                brier = ((p-one_hot)**2).sum(axis=1)
            else:
                brier = (p[:, 1]-labels)**2
            # Binary ECE calibrates p_positive; multiclass ECE calibrates the top class.
            error = predictions[selected] != labels
            conf, take = confidence[selected], accepted[selected]
            calibration = conf if bundle.get("task_kind") == "multiclass" else p[:,1]
            outcome = ~error if bundle.get("task_kind") == "multiclass" else labels
            known.add(brier, conf, error, take, calibration, outcome)
            scores.extend(conf[ood_pass[selected]].tolist())
            errors.extend(error[ood_pass[selected]].tolist())
            selected_groups = np.asarray(rows["group_id"])[selected]
            for group in np.unique(selected_groups):
                at = selected_groups == group
                by_group.setdefault(group, Metrics()).add(brier[at], conf[at], error[at], take[at], calibration[at], outcome[at])
            if "rare" in rows:
                if any(type(value) is not bool for value in rows["rare"]):
                    raise ValueError("Rare-population assessment must be explicitly boolean")
                at = np.asarray(rows["rare"], dtype=bool)[selected]
                rare.add(brier[at], conf[at], error[at], take[at], calibration[at], outcome[at])
        selected_ood = np.array([role == "ood_test" for role in rows["split"]])
        ood_events += int(selected_ood.sum())
        ood_accepted += int(accepted[selected_ood].sum())
    if not known.events or any(not groups[role] for role in ROLES[:-1]) or (ood is not None and not ood_events):
        raise ValueError("Missing independent train/calibration/selection/known-test/OOD cohorts")
    metrics = known.report()
    validation_metrics = {key: metrics[key] for key in ("brier", "ece", "risk", "coverage")}
    validation_metrics["ood_false_accept_rate"] = ood_accepted/ood_events if ood is not None else None
    candidate = {key: value for key, value in candidate.items() if key != "validation"}
    validation = {"schema_version": 1, "artifact_type": "classifier_decision_validation",
                  "evaluation_sha256": file_digest(path),
                  "evaluator_sha256": file_digest(__file__),
                  "policy_sha256": digest(candidate), "groups": {role: sorted(ids) for role, ids in groups.items()},
                  "known_events": known.events, "ood_events": ood_events,
                  "metrics": validation_metrics, "limits": limits}
    diagnostics = {"ece_bins": ECE_BINS, "ece_weighting": "event-weighted; binary positive probability or multiclass predicted-class confidence",
                   "known": metrics, "rare": rare.report(),
                   "by_group": {group: value.report() for group, value in sorted(by_group.items())},
                   "risk_coverage": risk_coverage(np.asarray(scores), np.asarray(errors), known.events),
                   "ood_events": ood_events, "ood_accepted": ood_accepted}
    return validation, diagnostics


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--events", type=Path, required=True)
    parser.add_argument("--policy", type=Path, required=True)
    parser.add_argument("--bundle-manifest", type=Path, required=True)
    parser.add_argument("--limits", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    candidate = json.loads(args.policy.read_text())
    if file_digest(args.bundle_manifest) != candidate["bundle_manifest_sha256"]:
        raise ValueError("Bundle manifest checksum mismatch")
    if args.bundle_manifest.suffix == ".json":
        bundle = json.loads(args.bundle_manifest.read_text())
    else:
        import yaml
        bundle = yaml.safe_load(args.bundle_manifest.read_text())
    validation, diagnostics = evaluate(args.events, candidate, bundle, json.loads(args.limits.read_text()))
    args.output.mkdir(parents=True, exist_ok=True)
    for name, report in [("validation.json", validation), ("evaluation.json", diagnostics)]:
        with (args.output/name).open("x") as stream:
            json.dump(report, stream, indent=2, allow_nan=False)
            stream.write("\n")


if __name__ == "__main__":
    main()
