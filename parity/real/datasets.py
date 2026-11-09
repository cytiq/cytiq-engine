"""Materialise the real-data registry (parity/real/datasets.json).

Usage: datasets.py <out_dir>

Copies every dataset's files into <out_dir>/<name>/ and checks each file's
SHA-256. A file comes from the R library in R_LIBS_USER (`r_package` +
`path`), from a `url`, or is a `member` of the dataset's zip `archive`;
downloads and extracted members are cached in parity/.data/real-cache by
SHA-256. A dataset's `controls` table (single-stain controls for the
`autospill` chain) is written as `controls_full.csv` (R autospill: filename,
dye, antigen, wavelength) and `controls.csv` (cytiq: filename, dye); an
`autospectral_cytometer` (the `autospectral` chain) is written to
`cytometer.txt`. File names may hold a directory (`controls/x.fcs`). Prints
one `name<TAB>chain` line per dataset that is ready. A missing or mismatching
file fails the run, except for an `optional` dataset whose download fails,
which is skipped with a warning. A `slow` dataset is skipped unless
CYTIQ_REAL_SLOW=1.
"""

import csv
import hashlib
import json
import os
import shutil
import subprocess
import sys
import zipfile
from pathlib import Path

PARITY = Path(__file__).resolve().parent.parent
CACHE = PARITY / ".data" / "real-cache"


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def download(url: str, digest: str) -> Path:
    cached = CACHE / digest
    if not cached.exists():
        CACHE.mkdir(parents=True, exist_ok=True)
        partial = cached.with_suffix(".part")
        # curl rather than urllib: it uses the system trust store, which
        # TLS-inspecting proxies add their root certificate to.
        fetched = subprocess.run(["curl", "-fsSL", "--retry", "3", "-o", str(partial), url], check=False)
        if fetched.returncode != 0:
            partial.unlink(missing_ok=True)
            raise OSError(f"curl {url} exited with {fetched.returncode}")
        actual = sha256(partial)
        if actual != digest:
            partial.unlink()
            raise ValueError(f"{url}: SHA-256 {actual}, expected {digest}")
        partial.rename(cached)
    return cached


def source(spec: dict, dataset: dict) -> Path:
    if "r_package" in spec:
        library = Path(os.environ["R_LIBS_USER"])
        return library / spec["r_package"] / spec["path"]
    if "member" not in spec:
        return download(spec["url"], spec["sha256"])
    cached = CACHE / spec["sha256"]
    if not cached.exists():
        path = download(dataset["archive"]["url"], dataset["archive"]["sha256"])
        partial = cached.with_suffix(".part")
        with zipfile.ZipFile(path) as bundle, bundle.open(spec["member"]) as member, partial.open("wb") as out:
            shutil.copyfileobj(member, out)
        partial.rename(cached)
    return cached


def materialise(dataset: dict, out_dir: Path) -> None:
    target = out_dir / dataset["name"]
    target.mkdir(parents=True, exist_ok=True)
    for name, spec in dataset["files"].items():
        path = source(spec, dataset)
        if not path.exists():
            raise FileNotFoundError(f"{path} (run parity/setup_env.sh)")
        actual = sha256(path)
        if actual != spec["sha256"]:
            raise ValueError(f"{path}: SHA-256 {actual}, expected {spec['sha256']}")
        (target / name).parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(path, target / name)
    if "controls" in dataset:
        write_controls(dataset["controls"], target)
    if "autospectral_cytometer" in dataset:
        (target / "cytometer.txt").write_text(dataset["autospectral_cytometer"] + "\n")


def write_controls(controls: list[dict], target: Path) -> None:
    for name, columns in (("controls_full.csv", ["filename", "dye", "antigen", "wavelength"]),
                          ("controls.csv", ["filename", "dye"])):
        with (target / name).open("w", newline="") as handle:
            writer = csv.DictWriter(handle, fieldnames=columns, extrasaction="ignore")
            writer.writeheader()
            writer.writerows(controls)


def main() -> None:
    out_dir = Path(sys.argv[1])
    registry = json.loads((Path(__file__).parent / "datasets.json").read_text())
    selected = set(filter(None, os.environ.get("CYTIQ_REAL_DATASETS", "").split(",")))
    if selected:
        selected |= {"flowsom_68983", "peacoqc_111", "compensaid_68983"}
    known = {dataset["name"] for dataset in registry["datasets"]}
    if selected - known:
        raise ValueError(f"unknown required datasets: {sorted(selected-known)}")
    for dataset in registry["datasets"]:
        if selected and dataset["name"] not in selected:
            continue
        if dataset.get("slow") and os.environ.get("CYTIQ_REAL_SLOW") != "1":
            print(f"skipping {dataset['name']}: slow, set CYTIQ_REAL_SLOW=1 to run it", file=sys.stderr)
            continue
        try:
            materialise(dataset, out_dir)
        except OSError as error:
            raise RuntimeError(f"required dataset {dataset['name']} unavailable: {error}") from error
        print(f"{dataset['name']}\t{dataset['chain']}")


if __name__ == "__main__":
    main()
