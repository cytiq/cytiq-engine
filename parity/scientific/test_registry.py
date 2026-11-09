import hashlib
from pathlib import Path
import tempfile
import unittest
from datasets import verify
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


if __name__ == "__main__": unittest.main()
