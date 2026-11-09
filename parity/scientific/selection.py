#!/usr/bin/env python3
"""Conservative selection: shared/numerical changes require full R parity."""
import argparse
from pathlib import PurePosixPath
import re
import subprocess


def relevant(paths):
    crates = {"fcs","density","transform","compensate","autospill","autospectral","spillqc","clean","peacoqc","mask","qc","cytoscan","flowsom","cytonorm","gating","population","classify","specimen","manifest","provenance"}
    for path in paths:
        parts = PurePosixPath(path).parts
        if path in ("Cargo.toml","Cargo.lock","mise.toml","mise.lock") or path.startswith(("parity/","data/fixtures/",".github/workflows/scientific.yml")):
            return True
        if len(parts) > 2 and parts[0] == "crates" and parts[1].removeprefix("cytiq-") in crates:
            return True
        if path.startswith(("crates/cytiq-engine/src/","crates/cytiq-engine/tests/")):
            return True
    return False


if __name__ == "__main__":
    parser=argparse.ArgumentParser()
    parser.add_argument("base"); parser.add_argument("head")
    args=parser.parse_args()
    if not all(re.fullmatch(r"[0-9a-f]{40}",value) for value in (args.base,args.head)):
        raise ValueError("selection requires two resolved commit SHAs")
    paths=subprocess.check_output(["git","diff","--name-only",args.base,args.head],text=True).splitlines()
    print("required="+str(relevant(paths)).lower())
