#!/usr/bin/env python3
"""Import immutable Pennsieve versions, then verify SHA-256-pinned local files.

`import-cymae` is an explicit dataset import, never part of a test/golden update.
It resolves public download URLs without persisting their signatures. A resumed
import verifies every cached file against its recorded digest and published size.
"""
from __future__ import annotations
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import sys

API = "https://api.pennsieve.io/discover/datasets/401/versions/1/files"


def sha256(path):
    h = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for block in iter(lambda: stream.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def request(url, payload=None):
    command = ["curl", "--fail", "--silent", "--show-error", "--retry", "3", "--max-time", "120", url]
    if payload is not None:
        command += ["--header", "content-type: application/json", "--data", json.dumps(payload)]
    return json.loads(subprocess.check_output(command))


def browse(path):
    offset = 0
    while True:
        from urllib.parse import urlencode
        page = request(API + "/browse?" + urlencode({**({"path":path} if path else {}), "limit": 100, "offset": offset}))
        for item in page["files"]:
            if item["type"] == "Directory":
                yield from browse(item["path"])
            else:
                yield item
        offset += len(page["files"])
        if offset >= page["totalCount"]:
            break
        if not page["files"]:
            raise RuntimeError("incomplete Pennsieve pagination")


def verify(registry, cache):
    if not registry.get("complete"):
        raise ValueError("required dataset import is incomplete")
    if not registry["files"]:
        raise ValueError("empty required dataset registry")
    for item in registry["files"]:
        path = cache / item["path"]
        if path.stat().st_size != item["bytes"] or sha256(path) != item["sha256"]:
            raise ValueError(f"dataset checksum mismatch: {path}")


def import_cymae(lock, cache):
    previous = json.loads(lock.read_text()) if lock.exists() else {"files": []}
    known = {x["path"]: x for x in previous["files"]}
    files = []
    for item in list(browse("")):
        relative = Path(item["path"])
        if relative.is_absolute() or ".." in relative.parts:
            raise ValueError("unsafe published path")
        path = cache / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        pin = known.get(item["path"])
        valid = pin and path.exists() and path.stat().st_size == pin["bytes"] and sha256(path) == pin["sha256"]
        if not valid:
            manifest = request(API + "/download-manifest", {"paths": [item["path"]]})
            if len(manifest["data"]) != 1:
                raise ValueError("download manifest did not select exactly one file")
            url = manifest["data"][0]["url"]
            temporary = path.with_suffix(path.suffix + ".partial")
            subprocess.run(["curl", "--fail", "--silent", "--show-error", "--retry", "3", "--max-time", "1800", "--output", str(temporary), url], check=True)
            if temporary.stat().st_size != item["size"]:
                raise ValueError(f"published size mismatch: {relative}")
            temporary.replace(path)
        record = {"path": item["path"], "bytes": item["size"], "s3_version": item.get("s3Version"), "sha256": sha256(path)}
        if pin and record != pin:
            raise ValueError(f"immutable dataset changed: {relative}")
        files.append(record)
        registry = {"schema_version": 1, "dataset": "cymae_401", "version": 1, "doi": "10.26275/qhne-f89h", "license": "CC-BY-4.0", "files": files}
        # A partial import is never accepted by verify: require the complete
        # published list below before copying this candidate to the repository.
        registry["complete"] = False
        lock.parent.mkdir(parents=True, exist_ok=True)
        lock.write_text(json.dumps(registry, indent=2) + "\n")
        print(f"{len(files)} {item['path']} {record['sha256']}", flush=True)
    registry["complete"] = True
    lock.write_text(json.dumps(registry, indent=2) + "\n")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("operation", choices=["import-cymae", "fetch", "verify"])
    parser.add_argument("--lock", type=Path, required=True)
    parser.add_argument("--cache", type=Path, required=True)
    args = parser.parse_args()
    if args.operation == "import-cymae":
        import_cymae(args.lock, args.cache)
    elif args.operation == "fetch":
        registry = json.loads(args.lock.read_text())
        if not registry.get("complete"): raise ValueError("incomplete dataset lock")
        for item in registry["files"]:
            path = args.cache / item["path"]
            if path.exists() and path.stat().st_size == item["bytes"] and sha256(path) == item["sha256"]: continue
            path.parent.mkdir(parents=True,exist_ok=True)
            manifest = request(API+"/download-manifest", {"paths":[item["path"]]})
            if len(manifest["data"]) != 1: raise ValueError("ambiguous download manifest")
            partial = path.with_suffix(path.suffix+".partial")
            subprocess.run(["curl","--fail","--silent","--show-error","--retry","3","--max-time","1800","--output",str(partial),manifest["data"][0]["url"]],check=True)
            if partial.stat().st_size != item["bytes"] or sha256(partial) != item["sha256"]: raise ValueError(f"dataset checksum mismatch: {item['path']}")
            partial.replace(path)
        verify(registry,args.cache)
    else:
        registry = json.loads(args.lock.read_text())
        if not registry.get("complete"):
            raise ValueError("required dataset import is incomplete")
        verify(registry, args.cache)


if __name__ == "__main__":
    main()
