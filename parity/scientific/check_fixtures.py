#!/usr/bin/env python3
"""Verify committed scientific inputs/goldens without R or Python dependencies."""
import hashlib
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def check(lock_path=ROOT / "data/fixtures/parity/sha256.json"):
    lock = json.loads(lock_path.read_text())
    if lock.get("schema_version") != 1 or not lock.get("files"):
        raise ValueError("invalid/empty scientific fixture lock")
    for relative, digest in lock["files"].items():
        path = ROOT / relative
        if hashlib.sha256(path.read_bytes()).hexdigest() != digest:
            raise ValueError(f"scientific fixture changed: {relative}; review upstream provenance before updating the lock")
    expected = set(lock["files"])
    actual = {str(path.relative_to(ROOT)) for path in lock_path.parent.rglob("*")
              if path.is_file() and path.name not in ("README.md", "sha256.json")}
    if expected != actual:
        raise ValueError(f"scientific fixture inventory changed: {sorted(expected ^ actual)}")
    print(f"verified {len(expected)} pinned scientific fixtures")


if __name__ == "__main__":
    check()
    check(ROOT / "data/fixtures/scientific/sha256.json")
