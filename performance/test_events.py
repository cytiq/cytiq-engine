"""Fast checks of the events benchmark's decision rule and reports; no
workloads, DuckDB or PyArrow."""

from pathlib import Path
import tempfile
import unittest

import events as benchmark


def result(variant, projection=1.0, scan=1.0, size=100, rss=10, layout=None):
    return {
        "variant": variant, "layout": layout or variant.split("-", 1)[0],
        "duckdb_projection_seconds": projection, "duckdb_scan_seconds": scan,
        "pyarrow_projection_seconds": 0.1, "pyarrow_read_seconds": 0.2,
        "bytes": size, "compression_ratio": 2.0, "events_per_second": 1e6,
        "export": {"wall_seconds": 1.0, "peak_rss_bytes": rss},
        "projected_mean": 1.5, "scan_total": 10.0, "scan_count": 4, "rows": 4,
        "values_sha256": "a" * 64,
    }


class EventsBenchmarkTests(unittest.TestCase):
    def test_wide_stays_canonical_unless_the_list_layout_wins_by_the_factor(self):
        rows = [result("wide-zstd1-rg256k"), result("list-zstd1-rg256k", projection=0.7,
                                                    size=70)]
        self.assertEqual(benchmark.decide(rows)["layout"]["canonical"], "wide")
        rows[1] = result("list-zstd1-rg256k", projection=0.6, size=100)
        decision = benchmark.decide(rows)["layout"]
        self.assertTrue(decision["list_wins"])
        self.assertNotEqual(decision["canonical"], "wide")
        rows[1] = result("list-zstd1-rg256k", projection=1.0, size=60)
        self.assertTrue(benchmark.decide(rows)["layout"]["list_wins"])

    def test_settings_prefer_the_smallest_row_group_and_level_within_ten_percent(self):
        rows = [
            result("wide-zstd1-rg256k", projection=1.05, scan=1.0, rss=30),
            result("wide-zstd3-rg256k", projection=1.0, scan=1.0, rss=30),
            result("wide-zstd1-rg128k", projection=1.2, scan=1.0, rss=20),
            result("wide-zstd1-rg1m", projection=1.0, scan=0.95, rss=90),
            result("wide-snappy-rg256k", projection=0.5, scan=0.5, rss=10),
        ]
        # Snappy is not a zstd candidate; 128k is more than 10% slower.
        self.assertEqual(benchmark.decide(rows)["settings"], "wide-zstd1-rg256k")
        rows[2] = result("wide-zstd1-rg128k", projection=1.08, scan=1.0, rss=20)
        self.assertEqual(benchmark.decide(rows)["settings"], "wide-zstd1-rg128k")

    def test_split_bests_fall_back_to_the_combined_score(self):
        rows = [
            result("wide-zstd1-rg256k", projection=1.0, scan=1.6, rss=30),
            result("wide-zstd1-rg1m", projection=1.6, scan=1.0, rss=60),
            result("wide-zstd3-rg256k", projection=1.05, scan=1.12, rss=40),
        ]
        decision = benchmark.decide(rows)
        self.assertEqual(decision["rule"], "combined projection and scan within 10%")
        self.assertEqual(decision["settings"], "wide-zstd3-rg256k")

    def test_disagreeing_values_or_sums_fail_the_run(self):
        rows = [result("wide-zstd1-rg256k"), result("wide-zstd3-rg256k")]
        benchmark.check_agreement(rows)
        rows[1]["values_sha256"] = "b" * 64
        with self.assertRaises(RuntimeError):
            benchmark.check_agreement(rows)
        rows = [result("wide-zstd1-rg256k"), result("list-zstd1-rg256k")]
        rows[1]["scan_total"] = 10.1
        with self.assertRaises(RuntimeError):
            benchmark.check_agreement(rows)
        rows[1]["scan_total"] = 10.0
        rows[1]["rows"] = 3
        with self.assertRaises(RuntimeError):
            benchmark.check_agreement(rows)

    def test_real_files_report_their_data_bytes_from_header_or_text(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "a.fcs"
            text = "|$BEGINDATA|200|$ENDDATA|299|"
            header = f"FCS3.1    {58:>8}{57 + len(text):>8}{0:>8}{0:>8}{0:>8}{0:>8}"
            path.write_bytes(header.encode() + text.encode() + b"\0" * 400)
            self.assertEqual(benchmark.real_workload(path)["data_bytes"], 100)
            header = f"FCS3.1    {58:>8}{57 + len(text):>8}{200:>8}{263:>8}{0:>8}{0:>8}"
            path.write_bytes(header.encode() + text.encode() + b"\0" * 400)
            self.assertEqual(benchmark.real_workload(path)["data_bytes"], 64)

    def test_the_summary_table_has_one_row_per_variant(self):
        rows = [result("wide-zstd1-rg256k"), result("list-zstd1-rg256k")]
        rows[0]["verify"] = {"wall_seconds": 0.5}
        lines = benchmark.table(rows).splitlines()
        self.assertEqual(len(lines), 4)
        self.assertIn("0.50", lines[2])

    def test_every_variant_names_its_layout_codec_and_row_group(self):
        names = [variant["name"] for variant in benchmark.VARIANTS]
        self.assertIn(benchmark.REFERENCE, names)
        self.assertEqual(len(names), len(set(names)))
        for variant in benchmark.VARIANTS:
            self.assertIn(variant["layout"], ("wide", "list"))
            self.assertIn("-rg", variant["name"])


if __name__ == "__main__":
    unittest.main()
