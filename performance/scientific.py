#!/usr/bin/env python3
"""Paired scientific-command benchmarks with exact output comparison.

Case manifests supply CLI arguments and immutable input files. {output} is the
same path for both builds; timing includes the command and all its I/O. Only
new diagnostic fields and volatile provenance times are excluded from equality.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import time

from run import measure, summarize, sha256, host_metadata

ADDITIONS = {"reference_diagnostics", "detector_residual_rms", "relative_residual_rms", "component_negative_fraction"}


def equal_json(a, b, path=(), diagnostics=False):
    if isinstance(a,dict) and isinstance(b,dict):
        extra = set(b)-set(a)
        allowed = ({"reference_diagnostics"} if path == () else ADDITIONS-{"reference_diagnostics"} if len(path)==2 and path[0]=="samples" else set()) if diagnostics else set()
        if not extra <= allowed or set(a)-set(b):
            raise ValueError(f"unexpected JSON schema change at {path}: {extra}")
        for key in a:
            if key in ("started_at_unix_ms","finished_at_unix_ms"): continue
            if key == "execution_fingerprint" and path == (): continue
            if key == "run_id" and path == (): continue
            if key == "git_commit" and path[-1:] == ("build",): continue
            if key in ("sha256","bytes") and "outputs" in path: continue
            equal_json(a[key],b[key],(*path,key),diagnostics)
    elif isinstance(a,list) and isinstance(b,list):
        if len(a) != len(b): raise ValueError(f"list size differs at {path}")
        for i,(x,y) in enumerate(zip(a,b)): equal_json(x,y,(*path,str(i)),diagnostics)
    elif a != b:
        raise ValueError(f"scientific value differs at {path}: {a!r} != {b!r}")


def validate_provenance(directory, common, helper):
    subprocess.run([str(helper.resolve()),"verify","--input",str(directory)],check=True)
    records = list(directory.rglob("*provenance.json"))
    if not records: raise ValueError("missing benchmark provenance")
    for path in records:
        record=json.loads(path.read_text())
        if record["build"]["profile"] != "release": raise ValueError("benchmark needs release builds")
        for output in record["outputs"]:
            original=Path(output["path"])
            actual=directory / original.relative_to(common)
            if actual.stat().st_size != output["bytes"] or sha256(actual) != output["sha256"]:
                raise ValueError(f"damaged output: {actual}")


def compare(a,b,common,helper):
    def files(root): return {p.relative_to(root) for p in root.rglob("*") if p.is_file() and not p.name.endswith(".lock") and not p.name.endswith(".tmp")}
    left,right=files(a),files(b)
    if left != right: raise ValueError(f"artifact inventory differs: {left ^ right}")
    validate_provenance(a,common,helper); validate_provenance(b,common,helper)
    for path in left:
        x,y=a/path,b/path
        if path.suffix == ".json":
            equal_json(json.loads(x.read_text()),json.loads(y.read_text()),diagnostics=path.name=="run.json" and path.parent.name=="diagnostics")
        elif path.suffix == ".parquet":
            import pyarrow.parquet as pq
            if not pq.read_table(x).equals(pq.read_table(y),check_metadata=True): raise ValueError(f"mask/model differs: {path}")
        elif sha256(x) != sha256(y):
            raise ValueError(f"output differs: {path}")


def main():
    parser=argparse.ArgumentParser()
    parser.add_argument("--manifest",type=Path,required=True)
    parser.add_argument("--baseline-bin",type=Path,required=True)
    parser.add_argument("--candidate-bin",type=Path,required=True)
    parser.add_argument("--helper",type=Path,required=True)
    parser.add_argument("--output",type=Path,required=True)
    parser.add_argument("--pairs",type=int,default=20)
    parser.add_argument("--warmups",type=int,default=2)
    args=parser.parse_args()
    if args.pairs < 1 or args.warmups < 0: raise ValueError("invalid pair counts")
    args.output=args.output.resolve(); args.output.mkdir(parents=True,exist_ok=False)
    manifest=json.loads(args.manifest.read_text())
    report={"schema_version":1,"host":host_metadata(),"manifest_sha256":sha256(args.manifest),"helper_sha256":sha256(args.helper),"runner_sha256":sha256(Path(__file__)),"binaries":{side:sha256(getattr(args,side+"_bin")) for side in ("baseline","candidate")},"cases":[],"status":"failed"}
    try:
        for case in manifest["cases"]:
            result={"name":case["name"],"input_sha256":{str(p):sha256(p) for p in case["inputs"]},"pairs":[]}
            report["cases"].append(result)
            root=args.output/case["name"]; root.mkdir()
            common=root/"work"
            for i in range(args.warmups+args.pairs):
                pair={}
                for side in (("baseline","candidate") if i%2==0 else ("candidate","baseline")):
                    common.mkdir()
                    command=[str(getattr(args,side+"_bin").resolve()),*[argument.replace("{output}",str(common)) for argument in case["arguments"]]]
                    pair[side]=measure(command,root/f"{i}-{side}",{**os.environ,"RAYON_NUM_THREADS":"4"})
                    saved=root/side
                    if saved.exists(): shutil.rmtree(saved)
                    common.rename(saved)
                    pair[side]["logical_output_bytes"]=sum(p.stat().st_size for p in saved.rglob("*") if p.is_file())
                    pair[side]["logical_input_bytes"]=sum(Path(p).stat().st_size for p in case["inputs"])
                compare(root/"baseline",root/"candidate",common,args.helper)
                if i >= args.warmups: result["pairs"].append(pair)
            acceptance=args.pairs==20 and args.warmups==2 and case.get("acceptance",False)
            result["summary"]=summarize(result["pairs"],acceptance)
            print(case["name"],json.dumps(result["summary"]),flush=True)
        complete=args.pairs==20 and args.warmups==2
        report["status"]=("passed" if complete else "diagnostic") if all(c["summary"]["acceptance"] is not False for c in report["cases"]) else "failed"
    except Exception as error:
        report["error"]=str(error)
        raise
    finally:
        (args.output/"results.json").write_text(json.dumps(report,indent=2)+"\n")
    if report["status"] == "failed": raise SystemExit(1)


if __name__ == "__main__": main()
