#!/usr/bin/env python3
"""SQL integration check of event labels and the analytics product.

Queries, with the pinned DuckDB, the analytics product (`cytiq dataset
export`) of two datasets:
- the cohort `crates/cytiq-engine/tests/lakehouse_dataset.rs` builds with
  the real commands: four subjects in two groups, one also on a second
  panel, QC, gate and FlowSOM labels of a shared model, `population
  observe` packages, and a classifier acquisition with abstentions;
- the reference dataset of `data/fixtures/dataset` (nested, overlapping and
  empty gates).

Every view of `views.sql` must run, and its answers must equal independent
oracles: PyArrow reading the label tables and the native files (masks,
memberships, predictions), and Python reading FlowSOM's CSV. Run it with the
interpreter of `parity/lakehouse/requirements.lock` (`just lakehouse-interop`).
"""

from __future__ import annotations

import argparse
from collections import Counter
import csv
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
KEYS = ("acquisition_id", "annotation_id", "source_event_id", "state")


class Checks:
    def __init__(self):
        self.failures = []
        self.passed = 0

    def that(self, condition, message):
        if condition:
            self.passed += 1
        else:
            self.failures.append(message)
            print(f"FAIL {message}", flush=True)


def sha256(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def cytiq(binary, *args, cwd=None):
    return subprocess.run([str(binary), *map(str, args)], capture_output=True, text=True,
                          check=False, cwd=cwd)


def connect(root, product):
    import duckdb

    connection = duckdb.connect()
    connection.execute("SET autoinstall_known_extensions = false")
    connection.execute(f"SET file_search_path = '{product},{root}'")
    connection.execute((product / "views.sql").read_text())
    return connection


def rows(connection, sql):
    return connection.execute(sql).fetchall()


def one(connection, sql):
    return connection.execute(sql).fetchone()[0]


def export_product(binary, manifest, output, root=None):
    if output.exists():
        shutil.rmtree(output)
    args = ["dataset", "export", "--manifest", manifest, "--output", output]
    if root is not None:
        args += ["--root", root]
    result = cytiq(binary, *args)
    if result.returncode:
        raise RuntimeError(f"dataset export of {manifest} failed: {result.stderr}")


def label_oracle(path):
    """Rows per state and per (column, value, state) of a label table."""
    import pyarrow.parquet as pq

    table = pq.read_table(path)
    states = table.column("state").to_pylist()
    counted = {}
    for field in table.schema:
        if field.name in KEYS or str(field.type) not in ("bool", "uint64", "string"):
            continue
        values = table.column(field.name).to_pylist()
        for value, state in zip(values, states):
            text = None if value is None else (str(value).lower() if isinstance(value, bool)
                                               else str(value))
            key = (field.name, text, state)
            counted[key] = counted.get(key, 0) + 1
    return Counter(states), counted


def check_every_view_runs(checks, name, connection, product):
    names = json.loads((product / "product.json").read_text())["views"]["names"]
    for view in names:
        try:
            one(connection, f'SELECT count(*) FROM "{view}"')
            checks.that(True, "")
        except Exception as error:  # noqa: BLE001 - reported as a failure
            checks.that(False, f"{name}: view {view} fails: {error}")
    return names


def counts_are_exact_integers(checks, name, connection, root):
    types = rows(connection, "SELECT DISTINCT typeof(count) FROM label_counts "
                             "UNION SELECT DISTINCT typeof(count) FROM label_states")
    checks.that(types == [("UBIGINT",)], f"{name}: label counts are UBIGINT, not {types}")
    for annotation, location in rows(connection, "SELECT annotation_id, location FROM labels"):
        states, counted = label_oracle(root / location)
        sql_states = dict(rows(connection, "SELECT state, count FROM label_states "
                                           f"WHERE annotation_id = '{annotation}'"))
        checks.that(sql_states == dict(states), f"{name}: states of {location}")
        sql_counts = {(c, v, s): n for c, v, s, n in rows(
            connection, "SELECT label_column, label_value, state, count FROM label_counts "
                        f"WHERE annotation_id = '{annotation}'")}
        checks.that(sql_counts == counted, f"{name}: counts of {location}")
        # Counting the rows in SQL gives the same integers.
        direct = dict(rows(connection, f"SELECT state, count(*) FROM read_parquet('{location}') "
                                       "GROUP BY state"))
        checks.that(direct == dict(states), f"{name}: SQL row counts of {location}")


def identities_are_unique_and_complete(checks, name, connection):
    for kind in ("qc", "gate", "cluster", "classifier"):
        view = f"labels_{kind}"
        if not one(connection, "SELECT count(*) FROM information_schema.tables "
                               f"WHERE table_name = '{view}'"):
            continue
        duplicates = one(connection, f"SELECT count(*) FROM (SELECT acquisition_id, annotation_id, "
                                     f"source_event_id FROM {view} GROUP BY ALL HAVING count(*) > 1)")
        checks.that(duplicates == 0, f"{name}: {view} names an event twice")
        # Every raw event of every labelled acquisition has exactly one row.
        missing = one(connection, f"SELECT count(*) FROM (SELECT annotation_id, count(*) AS n, "
                                  f"max(source_event_id) AS top FROM {view} GROUP BY ALL) v "
                                  "JOIN labels l USING (annotation_id) "
                                  "WHERE v.n != l.raw_events OR v.top + 1 != l.raw_events")
        checks.that(missing == 0, f"{name}: {view} misses raw events")


def joins_survive_reordering_and_filtering(checks, connection):
    for acquisition, events_view in rows(connection, """
            SELECT DISTINCT y.acquisition_id, 'events_' || y.panel_id || '_raw'
            FROM layers y JOIN labels l USING (acquisition_id)
            WHERE l.kind = 'gate' AND y.state = 'raw'"""):
        expected = one(connection, f"SELECT raw_events FROM acquisitions "
                                   f"WHERE acquisition_id = '{acquisition}'")
        shuffled = one(connection, f"""
            SELECT count(*) FROM (SELECT * FROM "{events_view}" ORDER BY random()) e
            JOIN labels_gate g USING (acquisition_id, source_event_id)
            WHERE e.acquisition_id = '{acquisition}'""")
        checks.that(shuffled == expected, f"shuffled events join gate labels of {acquisition}")
        accepted = one(connection, f"""
            SELECT count(*) FROM "{events_view}" e
            JOIN labels_qc q USING (acquisition_id, source_event_id)
            JOIN labels_gate g USING (acquisition_id, source_event_id)
            WHERE e.acquisition_id = '{acquisition}' AND q.state = 'accepted'""")
        assessed = one(connection, f"""
            SELECT count(*) FROM labels_gate WHERE acquisition_id = '{acquisition}'
            AND state = 'assessed'""")
        checks.that(accepted == assessed,
                    f"QC-accepted events are exactly the gate-assessed ones of {acquisition}")


def refused_duplicate_identity(checks, binary, root, work):
    """A membership naming one raw event twice is refused and publishes nothing."""
    import pyarrow as pa
    import pyarrow.parquet as pq

    work.mkdir(parents=True, exist_ok=True)
    source = root / "A1.membership.parquet"
    table = pq.read_table(source)
    ids = table.column("source_event_id").to_pylist()
    ids[1] = ids[0]
    tampered = table.set_column(table.schema.get_field_index("source_event_id"),
                                table.schema.field("source_event_id"),
                                pa.array(ids, type=pa.uint64()))
    native = work / "duplicate.parquet"
    pq.write_table(tampered, native)
    record = json.loads((root / "A1.counts.json.provenance.json").read_text())
    record["outputs"].append({"path": str(native), "bytes": native.stat().st_size,
                              "sha256": sha256(native)})
    producer = work / "duplicate.provenance.json"
    producer.write_text(json.dumps(record))
    output = work / "duplicate.labels.parquet"
    result = cytiq(binary, "export", "labels", "--kind", "gate", "--input", native,
                   "--producer-provenance", producer, "--events", root / "A1.raw.parquet",
                   "--qc-mask", root / "A1.analysis.parquet", "--gating-ml",
                   ROOT / "data/fixtures/specimen/gates.xml", "--output", output)
    checks.that(result.returncode == 2 and not output.exists(),
                f"a duplicate raw event is refused with exit 2 ({result.returncode})")


def gates_are_nested_and_overlap(checks, name, connection):
    outside = one(connection, """
        SELECT count(*) FROM gate_membership child
        JOIN label_columns c ON c.annotation_id = child.annotation_id AND c.label_column = child.gate
        JOIN gate_membership parent ON parent.annotation_id = child.annotation_id
          AND parent.source_event_id = child.source_event_id AND parent.gate = c.parent
        WHERE child.member AND NOT parent.member""")
    checks.that(outside == 0, f"{name}: no gate member is outside its parent")
    undecided = one(connection, """
        SELECT count(*) FROM gate_membership WHERE (state = 'assessed') = (member IS NULL)""")
    checks.that(undecided == 0, f"{name}: gate cells are null exactly when not assessed")


def reference_gates(checks, connection):
    members = dict(rows(connection, "SELECT gate, count(*) FILTER (WHERE member) "
                                    "FROM gate_membership GROUP BY gate"))
    checks.that(members == {"cells": 3, "CD45+": 2, "CD34+": 2, "blasts": 1, "large": 0},
                f"reference gate members {members}")
    both = one(connection, """
        SELECT count(*) FROM gate_membership a JOIN gate_membership b
          USING (annotation_id, source_event_id)
        WHERE a.gate = 'CD45+' AND b.gate = 'CD34+' AND a.member AND b.member""")
    checks.that(both == 1, "reference CD45+ and CD34+ overlap in one event")


def abstentions_and_nulls_survive(checks, connection, root):
    import pyarrow.parquet as pq

    native = pq.read_table(root / "C1.classified/predictions.parquet").to_pydict()
    unknown = sum(1 for label in native["label"] if label == "unknown")
    unassessed = sum(1 for assessed in native["assessed"] if not assessed)
    abstained = rows(connection, """
        SELECT count(*), count(*) FILTER (WHERE label IS NULL AND abstain_reason = 'margin'
          AND p_blast IS NOT NULL) FROM labels_classifier WHERE state = 'abstained'""")[0]
    checks.that(abstained == (unknown, unknown),
                f"legacy unknown predictions are {unknown} margin abstentions: {abstained}")
    empty = one(connection, """
        SELECT count(*) FROM labels_classifier WHERE state IN ('not_assessed', 'qc_rejected')
        AND label IS NULL AND abstain_reason IS NULL AND p_blast IS NULL""")
    checks.that(empty == unassessed, "unassessed predictions keep only nulls")
    fraction_denominators = rows(connection, """
        SELECT DISTINCT denominator FROM sample_label_fractions WHERE kind = 'classifier'""")
    predicted = one(connection, """
        SELECT count(*) FROM labels_classifier WHERE state IN ('classified', 'abstained')""")
    checks.that(fraction_denominators == [(predicted,)],
                "classifier fractions keep abstentions in the denominator")


def counts_match_observe(checks, connection):
    mismatches = rows(connection, """
        SELECT o.observation_id, o.value_u64, c.count
        FROM observations o
        JOIN sources s ON s.annotations_sha256 = o.lineage_annotations_sha256
        JOIN bindings b ON b.annotations_sha256 = s.annotations_sha256
          AND b.target_id = o.target_id AND b.target_kind = o.target_kind
        LEFT JOIN label_counts c ON c.annotation_id = s.annotation_id
          AND c.label_column = b.label AND c.label_value = 'true' AND c.state = 'assessed'
        WHERE o.measure = 'observed_count' AND o.status = 'measured'
          AND o.value_u64 IS DISTINCT FROM coalesce(c.count, 0)""")
    checked = one(connection, "SELECT count(*) FROM observations WHERE measure = 'observed_count'")
    checks.that(checked > 0 and not mismatches,
                f"observe counts equal the SQL label counts: {mismatches}")
    fractions = rows(connection, """
        SELECT value_f64, value_u64_count, denominator_count FROM (
          SELECT f.value_f64, c.value_u64 AS value_u64_count, f.denominator_count
          FROM observations f JOIN observations c
            ON c.lineage_annotations_sha256 = f.lineage_annotations_sha256
            AND c.target_id = f.target_id AND c.measure = 'observed_count'
          WHERE f.measure = 'fraction' AND f.status = 'measured')""")
    exact = all(value == count / denominator for value, count, denominator in fractions)
    checks.that(fractions and exact, "observe fractions are count / denominator exactly")


def multi_panel_join_by_acquisition(checks, connection):
    pairs = rows(connection, """
        SELECT s1.acquisition_id, s2.acquisition_id FROM samples s1 JOIN samples s2
          ON s1.specimen_id = s2.specimen_id AND s1.panel_id < s2.panel_id
        WHERE s1.panel_id = 'P1' AND s2.panel_id = 'P2'""")
    checks.that(len(pairs) == 1, f"one specimen holds both panels: {pairs}")
    for p1, p2 in pairs:
        naive = one(connection, f"""
            SELECT count(*) FROM events_P1_raw e JOIN labels_gate g USING (source_event_id)
            WHERE e.acquisition_id = '{p1}' AND g.acquisition_id = '{p2}'""")
        keyed = one(connection, f"""
            SELECT count(*) FROM events_P1_raw e JOIN labels_gate g
              USING (acquisition_id, source_event_id)
            WHERE e.acquisition_id = '{p1}' AND g.acquisition_id = '{p2}'""")
        checks.that(naive > 0 and keyed == 0,
                    "row numbers repeat across panels; only the acquisition key keeps them apart")


def cohort_counts_subjects(checks, connection):
    import statistics

    per_subject = {}
    for subject, group, panel, value in rows(connection, """
            SELECT d.subject_id, d."group", o.panel_id, o.value_f64
            FROM (SELECT DISTINCT ON (observation_id) * FROM observations) o
            JOIN specimen_design d USING (specimen_id)
            WHERE o.target_id = 't_cells' AND o.measure = 'fraction' AND o.status = 'measured'"""):
        per_subject.setdefault((group, panel), {})[subject] = value
    for (group, panel), values in per_subject.items():
        n, mean = rows(connection, f"""
            SELECT n_subjects, mean FROM cohort_population
            WHERE "group" = '{group}' AND panel_id = '{panel}' AND target_id = 't_cells'
              AND measure = 'fraction'""")[0]
        checks.that(n == len(values) and abs(mean - statistics.fmean(values.values())) < 1e-15,
                    f"cohort {group}/{panel}: {n} subjects, mean {mean}")
    # One sample per subject, panel and definition: nothing is ambiguous,
    # and a subject measured on two panels counts once in each.
    ambiguous = one(connection, "SELECT sum(n_subjects_ambiguous) FROM cohort_population") + \
        one(connection, "SELECT sum(n_subjects_ambiguous) FROM cohort_labels")
    checks.that(ambiguous == 0, f"no subject is ambiguous: {ambiguous}")
    t_cells = dict(rows(connection, """
        SELECT panel_id, n_subjects FROM cohort_labels
        WHERE "group" = 'case' AND kind = 'gate' AND label_column = 'T'"""))
    checks.that(t_cells == {"P1": 2, "P2": 1}, f"case subjects per panel: {t_cells}")
    events = one(connection, "SELECT max(n_subjects) FROM cohort_population")
    subjects = one(connection, "SELECT count(*) FROM subjects")
    checks.that(events <= subjects, "a cohort n never exceeds the subjects")


def diffcyt_counts(checks, connection, root):
    metaclusters = one(connection, "SELECT max(metaclusters) FROM labels WHERE kind = 'cluster'")
    samples = one(connection, "SELECT count(DISTINCT acquisition_id) FROM labels "
                              "WHERE kind = 'cluster'")
    total = one(connection, "SELECT count(*) FROM diffcyt_counts")
    checks.that(total == metaclusters * samples,
                f"diffcyt counts are zero-filled: {total} = {metaclusters} x {samples}")
    sql = {(s, c): n for s, c, n in rows(connection,
                                        "SELECT sample_id, cluster_id, n_cells FROM diffcyt_counts")}
    oracle = {}
    for sample, run_id in rows(connection, """
            SELECT s.sample_id, regexp_extract(l.location, '^([A-Z0-9]+)\\.', 1)
            FROM labels l JOIN samples s USING (acquisition_id) WHERE l.kind = 'cluster'"""):
        counts = Counter(row["flowsom_metacluster"]
                         for row in csv.DictReader((root / f"{run_id}.clusters.csv").open()))
        for cluster in range(1, metaclusters + 1):
            oracle[(sample, str(cluster))] = counts.get(str(cluster), 0)
    checks.that(sql == oracle, "diffcyt counts equal FlowSOM's assignments per sample")


def deterministic(checks, binary, manifest, first, second):
    export_product(binary, manifest, second)
    for name in ("product.json", "views.sql"):
        checks.that(sha256(first / name) == sha256(second / name),
                    f"{name} is the same on a second export")


def run(args):
    import duckdb
    import pyarrow

    checks = Checks()
    root = args.dataset
    product = root / "product"
    connection = connect(root, product)
    check_every_view_runs(checks, "cohort", connection, product)
    counts_are_exact_integers(checks, "cohort", connection, root)
    identities_are_unique_and_complete(checks, "cohort", connection)
    joins_survive_reordering_and_filtering(checks, connection)
    refused_duplicate_identity(checks, args.cytiq, root, args.work / "refused")
    gates_are_nested_and_overlap(checks, "cohort", connection)
    abstentions_and_nulls_survive(checks, connection, root)
    counts_match_observe(checks, connection)
    multi_panel_join_by_acquisition(checks, connection)
    cohort_counts_subjects(checks, connection)
    diffcyt_counts(checks, connection, root)
    deterministic(checks, args.cytiq, root / "dataset.json", product, args.work / "again")

    reference = ROOT / "data/fixtures/dataset/reference"
    reference_product = args.work / "reference"
    export_product(args.cytiq, reference / "manifest.json", reference_product)
    connection = connect(reference, reference_product)
    check_every_view_runs(checks, "reference", connection, reference_product)
    counts_are_exact_integers(checks, "reference", connection, reference)
    identities_are_unique_and_complete(checks, "reference", connection)
    gates_are_nested_and_overlap(checks, "reference", connection)
    reference_gates(checks, connection)

    report = {"passed": checks.passed, "failures": checks.failures,
              "duckdb": duckdb.__version__, "pyarrow": pyarrow.__version__}
    args.work.mkdir(parents=True, exist_ok=True)
    (args.work / "labels.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))
    return 1 if checks.failures else 0


def parse_args(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cytiq", type=Path, default=ROOT / "target/release/cytiq")
    parser.add_argument("--dataset", type=Path, default=ROOT / "target/lakehouse/analytics",
                        help="the cohort lakehouse_dataset.rs wrote")
    parser.add_argument("--work", type=Path, default=ROOT / "target/lakehouse/labels")
    return parser.parse_args(argv)


if __name__ == "__main__":
    sys.exit(run(parse_args()))
