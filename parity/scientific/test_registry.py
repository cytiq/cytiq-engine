import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
from datasets import verify
from r_packages import dependency_closure, installation_order, restore
from selection import relevant


class ScientificRegistryTests(unittest.TestCase):
    def test_missing_corrupt_or_partial_required_inputs_fail(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory); (root/"events.fcs").write_bytes(b"fixed")
            lock={"complete":True,"files":[{"path":"events.fcs","bytes":5,"sha256":hashlib.sha256(b"fixed").hexdigest()}]}
            verify(lock,root)
            (root/"events.fcs").write_bytes(b"other")
            with self.assertRaises(ValueError): verify(lock,root)
            (root/"events.fcs").unlink()
            with self.assertRaises(FileNotFoundError): verify(lock,root)
            with self.assertRaises(ValueError): verify({"complete":False,"files":[]},root)

    def test_shared_numerics_and_dependencies_require_parity(self):
        for path in ("Cargo.lock","crates/cytiq-fcs/src/data.rs","crates/cytiq-autospectral/src/runner.rs","crates/cytiq-manifest/src/lib.rs","crates/cytiq-provenance/src/input.rs","parity/tolerances.json","data/fixtures/parity/versions.txt"):
            self.assertTrue(relevant([path]),path)
        self.assertFalse(relevant(["README.md",".github/ISSUE_TEMPLATE/bug.md"]))


class ReferenceEnvironmentTests(unittest.TestCase):
    @staticmethod
    def package(name, depends="", priority=""):
        return {"Package": name, "Depends": depends, "Priority": priority}

    @staticmethod
    def pin(name, dependencies=()):
        return {"name": name, "version": "1.0", "dependencies": list(dependencies),
                "url": "https://example.invalid/source", "sha256": "unused"}

    def test_closure_includes_system_and_recommended_dependencies(self):
        root = self.package("caTools", "R (>= 4.0), bitops, Matrix, methods")
        available = [root, self.package("bitops"),
                     self.package("Matrix", priority="recommended"),
                     self.package("methods", priority="base"), self.package("unrelated")]
        names = {p["Package"] for p in dependency_closure([root], available)}
        self.assertEqual(names, {"caTools", "bitops", "Matrix"})

    def test_closure_follows_transitive_dependencies_and_managed_precedence(self):
        root = self.package("root", "middle")
        middle = self.package("middle", "leaf")
        stale = self.package("middle", "unavailable")
        names = {p["Package"] for p in dependency_closure(
            [root], [root, middle, self.package("leaf"), stale])}
        self.assertEqual(names, {"root", "middle", "leaf"})

    def test_missing_dependency_metadata_fails_freezing(self):
        root = self.package("caTools", "bitops")
        with self.assertRaisesRegex(ValueError, "bitops required by caTools"):
            dependency_closure([root], [root])

    def test_incomplete_lock_fails_before_consulting_warm_environment(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            lock = root / "lock.json"
            lock.write_text(json.dumps({"base_packages": [],
                                        "packages": [self.pin("caTools", ["bitops"])]}))
            with patch("r_packages.inventory") as inventory, patch("r_packages.download") as download:
                with self.assertRaisesRegex(ValueError, "unlocked.*caTools.*bitops"):
                    restore(root / "library", lock, root, install=True)
                inventory.assert_not_called()
                download.assert_not_called()

    def test_cold_restore_installs_dependencies_before_dependants(self):
        document = {"r_version": "4.5.3", "base_packages": ["methods"],
                    "packages": [self.pin("caTools", ["bitops", "methods"]), self.pin("bitops")]}
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            lock = root / "lock.json"
            lock.write_text(json.dumps(document))
            with patch("r_packages.subprocess.check_output", return_value="4.5.3"), \
                    patch("r_packages.download"), patch("r_packages.subprocess.run") as install:
                restore(root / "empty-library", lock, root, install=True)
            sources = [Path(call.args[0][-1]).name for call in install.call_args_list]
            self.assertEqual(sources, ["bitops_1.0.tar.gz", "caTools_1.0.tar.gz"])

    def test_duplicate_packages_and_cycles_fail_before_installation(self):
        for pins in ([self.pin("a"), self.pin("a")],
                     [self.pin("a", ["b"]), self.pin("b", ["a"])]):
            with self.subTest(pins=pins), self.assertRaises(ValueError):
                installation_order({"base_packages": [], "packages": pins})

    def test_committed_lock_is_complete_for_a_cold_library(self):
        lock = json.loads((Path(__file__).resolve().parents[1] / "r-packages.lock.json").read_text())
        ordered = installation_order(lock)
        positions = {p["name"]: i for i, p in enumerate(ordered)}
        for pin in ordered:
            for dependency in pin["dependencies"]:
                if dependency in positions:
                    self.assertLess(positions[dependency], positions[pin["name"]])
        self.assertLess(positions["bitops"], positions["caTools"])
        self.assertLess(positions["XML"], positions["FlowSOM"])


if __name__ == "__main__": unittest.main()
