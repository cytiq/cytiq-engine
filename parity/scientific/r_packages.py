#!/usr/bin/env python3
"""Explicitly freeze and restore the complete installed parity R library.

Source tarballs are pinned by SHA-256. Installation uses dependency order from
DESCRIPTION, never a fresh dependency resolution against a moving repository.
"""
import argparse
from concurrent.futures import ThreadPoolExecutor
import csv
import io
import json
from pathlib import Path
import subprocess
from datasets import sha256

GIT = {"autospill": ("carlosproca/autospill", "1e60e86337b297f1dd9ffec6701d8010dd06175b"),
       "AutoSpectral": ("DrCytometer/AutoSpectral", "292dfeaae6b382d1a7c9c263c5bb4de35ff67207"),
       "CytoScan": ("AUMC-HEMA/CytoScan", "c97f403d3ad6c673bed67f1f7dff4062e2f28e19"),
       "CompensAID": ("Olsman/CompensAID", "db748036e977571e09ddb4267ca598a1d700f2fc"),
       "CytoNorm": ("saeyslab/CytoNorm", "278f352d8edffd7a5d3e6b0b76c402d991a1e7a9")}


def inventory(library):
    code = 'x <- installed.packages(lib.loc=commandArgs(TRUE)[1], fields=c("Repository","Depends","Imports","LinkingTo")); write.table(x[,c("Package","Version","Repository","Depends","Imports","LinkingTo")],stdout(),sep="\\t",row.names=FALSE,quote=TRUE,na="")'
    return list(csv.DictReader(io.StringIO(subprocess.check_output(["Rscript", "-e", code, str(library)], text=True)), delimiter="\t"))


def download(urls, path, expected=None):
    if path.exists() and expected and sha256(path) == expected: return
    receipt = path.with_suffix(path.suffix+".json")
    if path.exists() and receipt.exists() and expected is None:
        cached = json.loads(receipt.read_text())
        if cached["url"] in urls and sha256(path) == cached["sha256"]: return cached["url"]
    for url in urls:
        result = subprocess.run(["curl", "--fail", "--silent", "--show-error", "--location", "--retry", "2", "--max-time", "120", "--output", str(path)+".partial", url], capture_output=True)
        if result.returncode == 0:
            Path(str(path)+".partial").replace(path)
            if expected and sha256(path) != expected: raise ValueError(f"R source checksum mismatch: {path.name}")
            receipt.write_text(json.dumps({"url":url,"sha256":sha256(path)}))
            return url
    raise ValueError(f"pinned R source unavailable: {path.name}")


def freeze(library, lock, cache):
    packages = inventory(library)
    names = {p["Package"] for p in packages}
    import re
    def freeze_one(p):
        name, version = p["Package"], p["Version"]
        dependencies = sorted(set(re.findall(r"(?:^|,)\s*([A-Za-z][A-Za-z0-9.]*)", ",".join(p[k] for k in ("Depends","Imports","LinkingTo")))) & names)
        if name in GIT:
            repo, commit = GIT[name]
            urls = [f"https://codeload.github.com/{repo}/tar.gz/{commit}"]
        elif "bioc" in p["Repository"].lower():
            urls = [f"https://bioconductor.org/packages/3.22/{kind}/src/contrib/{name}_{version}.tar.gz" for kind in ("bioc", "data/experiment", "data/annotation")]
        else:
            urls = [f"https://cloud.r-project.org/src/contrib/{name}_{version}.tar.gz", f"https://cloud.r-project.org/src/contrib/Archive/{name}/{name}_{version}.tar.gz"]
        path = cache / f"{name}_{version}.tar.gz"
        url = download(urls,path)
        print(name, version, flush=True)
        return {"name":name,"version":version,"url":url,"sha256":sha256(path),"dependencies":dependencies}
    with ThreadPoolExecutor(max_workers=4) as executor:
        pins = list(executor.map(freeze_one, packages))
    document = {"schema_version":1,"r_version":"4.5.3","bioconductor":"3.22","packages":pins}
    lock.write_text(json.dumps(document,indent=2)+"\n")


def restore(library, lock, cache, install):
    document = json.loads(lock.read_text())
    version = subprocess.check_output(["Rscript","-e","cat(as.character(getRversion()))"],text=True)
    if version != document["r_version"]: raise ValueError("R version differs from lock")
    installed = {p["Package"]:p["Version"] for p in inventory(library)} if library.exists() else {}
    pending = {p["name"]:p for p in document["packages"] if installed.get(p["name"]) != p["version"]}
    if pending and not install: raise ValueError(f"R environment differs from lock: {sorted(pending)}")
    library.mkdir(parents=True,exist_ok=True)
    while pending:
        ready = [p for p in pending.values() if not set(p["dependencies"]) & pending.keys()]
        if not ready: raise ValueError("cyclic R source dependency lock")
        for pin in ready:
            source = cache / f"{pin['name']}_{pin['version']}.tar.gz"
            download([pin["url"]],source,pin["sha256"])
            subprocess.run(["R","CMD","INSTALL",f"--library={library}",str(source)],check=True)
            del pending[pin["name"]]


def main():
    parser=argparse.ArgumentParser()
    parser.add_argument("operation",choices=["freeze","verify","install"])
    parser.add_argument("--library",type=Path,required=True)
    parser.add_argument("--lock",type=Path,required=True)
    parser.add_argument("--cache",type=Path,default=Path("target/scientific/r-sources"))
    args=parser.parse_args()
    args.cache.mkdir(parents=True,exist_ok=True)
    if args.operation == "freeze": freeze(args.library,args.lock,args.cache)
    else: restore(args.library,args.lock,args.cache,args.operation=="install")


if __name__ == "__main__": main()
