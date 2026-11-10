#!/usr/bin/env python3
"""Paired, end-to-end command benchmarks; only the CLI child is measured."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import random
import shutil
import statistics
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
LARGE_EVENTS = 1_000_000
REAL_DATA = (
    ("flowsom", "FlowSOM/extdata/68983.fcs",
     "ccf75633447c7acec4e30174b82e79ec9ba5164981f05186328b62da4bc3896c"),
    ("peacoqc", "PeacoQC/extdata/111.fcs",
     "28b00aac4b3f5acb451e7b3318360ba2d57a50c9fd6b8c387abc3bcb05bdfa7e"),
)
REQUIRED = {"large_compensate"} | {
    f"large_{method}_t{threads}"
    for method in ("arcsinh", "logicle") for threads in (1, 4)
}


def sha256(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def capture(command):
    result = subprocess.run(command, text=True, capture_output=True, check=False, cwd=ROOT)
    return result.stdout.strip() if result.returncode == 0 else None


def host_metadata():
    def sysctl(name):
        return capture(["/usr/sbin/sysctl", "-n", name]) if sys.platform == "darwin" else None

    return {
        "platform": platform.platform(), "architecture": platform.machine(),
        "cpu": sysctl("machdep.cpu.brand_string") or platform.processor(),
        "physical_cpus": sysctl("hw.physicalcpu"), "logical_cpus": os.cpu_count(),
        "ram_bytes": sysctl("hw.memsize"), "rustc": capture(["rustc", "-Vv"]),
        "head": capture(["git", "rev-parse", "HEAD"]),
        "lock_sha256": sha256(ROOT / "Cargo.lock"),
        "workspace_manifest_sha256": sha256(ROOT / "Cargo.toml"),
        "tracked_diff_sha256": hashlib.sha256(subprocess.check_output(
            ["git", "diff", "--binary", "HEAD"], cwd=ROOT)).hexdigest(),
        "untracked_source_sha256": {
            relative: sha256(ROOT / relative) for relative in subprocess.check_output(
                ["git", "ls-files", "--others", "--exclude-standard"], cwd=ROOT,
                text=True).splitlines()
            if Path(relative).suffix in (".rs", ".py", ".md", ".yml")
        },
        "environment": {key: os.environ.get(key) for key in (
            "RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "RUSTC_WRAPPER", "RUSTUP_TOOLCHAIN",
            "CARGO_BUILD_TARGET", "CARGO_PROFILE_RELEASE_OPT_LEVEL", "CARGO_PROFILE_RELEASE_LTO",
            "CARGO_PROFILE_RELEASE_CODEGEN_UNITS", "CARGO_PROFILE_RELEASE_OVERFLOW_CHECKS",
        )},
    }


def rss_bytes(value, system=None):
    # wait4 reports bytes on macOS and KiB on Linux. Never use the cumulative
    # RUSAGE_CHILDREN high-water mark: that would include earlier children.
    system = sys.platform if system is None else system
    if system == "darwin":
        return int(value)
    if system.startswith("linux"):
        return int(value) * 1024
    raise RuntimeError(f"unsupported RSS units on {system}")


def measure(command, log_prefix, env):
    log_prefix.parent.mkdir(parents=True, exist_ok=True)
    with log_prefix.with_suffix(".stdout").open("wb") as stdout, \
            log_prefix.with_suffix(".stderr").open("wb") as stderr:
        started = time.perf_counter()
        with subprocess.Popen(command, stdout=stdout, stderr=stderr, env=env) as process:
            while True:
                try:
                    _, status, usage = os.wait4(process.pid, 0)
                    break
                except InterruptedError:
                    continue
            process.returncode = os.waitstatus_to_exitcode(status)
            elapsed = time.perf_counter() - started
        if process.returncode:
            raise RuntimeError(f"command exited {process.returncode}: {command}; "
                               f"see {log_prefix.with_suffix('.stderr')}")
    return {"wall_seconds": elapsed, "user_seconds": usage.ru_utime,
            "system_seconds": usage.ru_stime, "peak_rss_bytes": rss_bytes(usage.ru_maxrss),
            "input_blocks": usage.ru_inblock, "output_blocks": usage.ru_oublock}


def percentile(values, fraction):
    position = (len(values) - 1) * fraction
    lower = int(position)
    upper = min(lower + 1, len(values) - 1)
    return values[lower] + (values[upper] - values[lower]) * (position - lower)


def summarize(pairs, acceptance, profile="exact"):
    ratios = [pair["candidate"]["wall_seconds"] / pair["baseline"]["wall_seconds"]
              for pair in pairs]
    rng = random.Random(42)
    bootstraps = sorted(statistics.median(rng.choices(ratios, k=len(ratios)))
                        for _ in range(10_000))
    medians = {side: {metric: statistics.median(pair[side][metric] for pair in pairs)
                      for metric in pairs[0][side]}
               for side in ("baseline", "candidate")}
    rss_ratio = medians["candidate"]["peak_rss_bytes"] / medians["baseline"]["peak_rss_bytes"]
    interval = [percentile(bootstraps, 0.025), percentile(bootstraps, 0.975)]
    return {"medians": medians, "median_wall_ratio": statistics.median(ratios),
            "wall_ratio_ci95": interval, "peak_rss_ratio": rss_ratio,
            "acceptance": (rss_ratio <= (1.05 if profile == "refactor" else 0.90) and interval[1] <= 1.05) if acceptance else None}


def checked(command):
    subprocess.run([str(part) for part in command], check=True, cwd=ROOT)


def verify_outputs(helper, baseline, candidate, profile="exact"):
    command = [helper, "compare", "--baseline", baseline, "--candidate", candidate]
    if profile != "exact":
        command.extend(["--profile", profile])
    checked(command)


def provenance_build(directory):
    records = sorted(directory.rglob("*provenance.json"))
    if not records:
        raise RuntimeError(f"missing provenance in {directory}")
    builds = [json.loads(path.read_text())["build"] for path in records]
    if builds[0].get("profile") != "release":
        raise RuntimeError(f"benchmark requires a release binary: {directory}")
    if any(build != builds[0] for build in builds):
        raise RuntimeError(f"inconsistent builds in {directory}")
    return builds[0]


def cases_for(name, inputs, events, methods=("arcsinh", "logicle"), threads=(1, 4)):
    cases = []
    if len(inputs) == 1:
        cases.append({"name": f"{name}_compensate", "stage": "compensate",
                      "inputs": inputs, "events": events})
    for method in methods:
        for count in threads:
            cases.append({"name": f"{name}_{method}_t{count}", "stage": "transform",
                          "method": method, "threads": count, "inputs": inputs, "events": events})
    return cases


def prepare(args, helper):
    data = args.output / "data"
    data.mkdir(parents=True, exist_ok=True)
    cases = []
    for name, events in (("small", 100_000), ("large", LARGE_EVENTS)):
        if args.smoke:
            events = 8193 if name == "small" else 16_385
        raw = data / f"{name}.fcs"
        checked([helper, "generate", "--output", raw, "--events", events, "--seed", 42])
        cases.extend(cases_for(name, [raw], events))
    inputs = []
    multi_events = 257 if args.smoke else 100_000
    for index in range(4):
        raw = data / f"multi_{index}.fcs"
        checked([helper, "generate", "--output", raw, "--events", multi_events,
                 "--seed", 100 + index])
        inputs.append(raw)
    cases.extend(cases_for("multi", inputs, multi_events * 4, threads=(4,)))
    if not args.smoke and not args.synthetic_only:
        for name, relative, digest in REAL_DATA:
            source = args.r_library / relative
            if not source.is_file() or sha256(source) != digest:
                raise RuntimeError(f"missing or unverified real dataset: {source}; "
                                   "run just parity-setup or supply --r-library")
            info = subprocess.check_output([str(helper), "inspect", "--input", str(source)], text=True)
            cases.extend(cases_for(name, [source], json.loads(info)["events"]))
    return cases


def command_for(case, binary, inputs, work):
    command = [str(binary), case["stage"]]
    if case["stage"] == "compensate":
        command.extend(["--input", str(inputs[0]), "--output", str(work / "result.fcs")])
    else:
        for path in inputs:
            command.extend(["--input", str(path)])
        command.extend(["--output", str(work / "result.fcs" if len(inputs) == 1 else work / "files"),
                        "--method", case["method"], "--threads", str(case["threads"]),
                        "--metadata-json", str(work / "metrics.json")])
    command.append("--overwrite")
    return command


def write_report(report, output):
    (output / "results.json").write_text(json.dumps(report, indent=2) + "\n")
    lines = ["# transform / compensate performance", "", f"Status: **{report['status']}**", "",
             "| Case | Wall ratio | 95% CI | Peak RSS ratio | Gate |",
             "| --- | ---: | --- | ---: | --- |"]
    for case in report["cases"]:
        result = case.get("summary")
        if result is None:
            continue
        lower, upper = result["wall_ratio_ci95"]
        gate = {True: "pass", False: "FAIL", None: "diagnostic"}[result["acceptance"]]
        lines.append(f"| {case['name']} | {result['median_wall_ratio']:.3f} | "
                     f"{lower:.3f}–{upper:.3f} | {result['peak_rss_ratio']:.3f} | {gate} |")
    if "error" in report:
        lines.extend(["", f"Error: {report['error']}"])
    lines.extend(["", "| Case | Build | Wall (s) | CPU user + system (s) | Peak RSS (MiB) | Events/s |",
                  "| --- | --- | ---: | ---: | ---: | ---: |"])
    for case in report["cases"]:
        if "summary" not in case:
            continue
        for side, values in case["summary"]["medians"].items():
            cpu = values["user_seconds"] + values["system_seconds"]
            lines.append(f"| {case['name']} | {side} | {values['wall_seconds']:.4f} | "
                         f"{cpu:.4f} | {values['peak_rss_bytes'] / (1024 * 1024):.1f} | "
                         f"{values['events_per_second']:.0f} |")
    lines.extend(["", "Ratios are candidate / baseline. Full metrics and per-pair samples: results.json.",
                  ("Refactor profile: peak RSS ≤1.05 and upper wall-time CI ≤1.05."
                   if report["settings"].get("profile", "exact") == "refactor"
                   else "Only large cases gate acceptance: RSS ≤0.90 and upper wall-time CI ≤1.05."),
                  "Warm-cache measurements include reading, hashing, publication and provenance; "
                  "they do not measure durable disk flush latency."])
    (output / "scorecard.md").write_text("\n".join(lines) + "\n")


def run(args):
    if args.output.exists():
        raise ValueError(f"output already exists: {args.output}; select a fresh --output directory")
    args.output.mkdir(parents=True)
    profile = getattr(args, "profile", "exact")
    report = {"schema_version": 1, "status": "running", "host": host_metadata(),
              "settings": {"profile": profile, "pairs": args.pairs, "warmups": args.warmups,
                           "bootstrap_samples": 10_000, "bootstrap_seed": 42,
                           "generator_version": 1, "single_input_seed": 42,
                           "multi_input_seeds": [100, 101, 102, 103],
                           "rayon_num_threads": 4,
                           "smoke": args.smoke, "synthetic_only": args.synthetic_only},
              "binaries": {side: {"path": str(binary), "sha256": sha256(binary),
                                  "build_manifest": (json.loads((binary.parent / "build.json").read_text())
                                                     if (binary.parent / "build.json").is_file() else None)}
                           for side, binary in (("baseline", args.baseline_bin),
                                                ("candidate", args.candidate_bin))},
              "cases": []}
    try:
        helper = args.helper_bin
        if not helper.is_file():
            raise RuntimeError("build the helper first: cargo build --release --locked "
                               "-p cytiq-engine --example performance_support")
        report["helper_sha256"] = sha256(helper)
        cases = prepare(args, helper)
        if args.case:
            cases = [case for case in cases if case["name"] in args.case]
            unknown = set(args.case) - {case["name"] for case in cases}
            if unknown:
                raise ValueError(f"unknown cases: {sorted(unknown)}")
        report["datasets"] = {str(path): sha256(path)
                              for case in cases for path in case["inputs"]}
        env = dict(os.environ, RAYON_NUM_THREADS="4")
        for case in cases:
            print(f"{case['name']}: preparing", flush=True)
            case_dir = args.output / case["name"]
            case_dir.mkdir()
            inputs = case["inputs"]
            if case["stage"] == "transform":
                prepared = case_dir / "inputs"
                prepared.mkdir()
                transformed_inputs = []
                for index, source in enumerate(inputs):
                    dest = prepared / f"{index}.comp.fcs"
                    checked([args.baseline_bin, "compensate", "--input", source, "--output", dest])
                    transformed_inputs.append(dest)
                inputs = transformed_inputs
            result = {key: value for key, value in case.items() if key != "inputs"}
            result["inputs"] = {str(path): sha256(path) for path in inputs}
            result["pairs"] = []
            report["cases"].append(result)
            work = case_dir / "work"
            for iteration in range(args.warmups + args.pairs):
                pair = {}
                sides = ("baseline", "candidate") if iteration % 2 == 0 else ("candidate", "baseline")
                for side in sides:
                    archive = case_dir / side
                    if archive.exists():
                        shutil.rmtree(archive)
                    work.mkdir()
                    binary = getattr(args, f"{side}_bin")
                    sample = measure(command_for(case, binary, inputs, work),
                                     case_dir / "logs" / f"{iteration}_{side}", env)
                    sample["events_per_second"] = case["events"] / sample["wall_seconds"]
                    pair[side] = sample
                    work.rename(archive)
                verify_outputs(helper, case_dir / "baseline", case_dir / "candidate", profile)
                for side in ("baseline", "candidate"):
                    build = provenance_build(case_dir / side)
                    previous = report["binaries"][side].setdefault("build", build)
                    if previous != build:
                        raise RuntimeError(f"build changed during run: {side}")
                if iteration >= args.warmups:
                    result["pairs"].append(pair)
                write_report(report, args.output)
                print(f"{case['name']}: {iteration + 1}/{args.warmups + args.pairs} pairs, parity {profile}",
                      flush=True)
            gating = (case["name"] in REQUIRED and not args.smoke
                      and args.pairs >= 20 and args.warmups >= 2)
            result["summary"] = summarize(result["pairs"], gating, profile)
        complete = (REQUIRED <= {case["name"] for case in report["cases"]}
                    and not args.smoke and args.pairs >= 20 and args.warmups >= 2)
        failed = any(case["summary"]["acceptance"] is False for case in report["cases"])
        report["status"] = "failed" if failed else "passed" if complete else "diagnostic"
        write_report(report, args.output)
        print(f"{report['status']}: {args.output / 'scorecard.md'}", flush=True)
        return 1 if failed else 0
    except Exception as error:
        report["status"] = "failed"
        report["error"] = str(error)
        write_report(report, args.output)
        raise


def parse_args():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=("exact", "refactor"), default="exact")
    parser.add_argument("--baseline-bin", type=Path, required=True)
    parser.add_argument("--candidate-bin", type=Path, required=True)
    parser.add_argument("--helper-bin", type=Path,
                        default=ROOT / "target/release/examples/performance_support")
    parser.add_argument("--output", type=Path,
                        default=ROOT / "performance/.out" / time.strftime("%Y%m%d-%H%M%S"))
    parser.add_argument("--r-library", type=Path, default=ROOT / "parity/.env/Rlib")
    parser.add_argument("--pairs", type=int, default=20)
    parser.add_argument("--warmups", type=int, default=2)
    parser.add_argument("--case", action="append", help="run only these named cases")
    parser.add_argument("--synthetic-only", action="store_true")
    parser.add_argument("--smoke", action="store_true", help="small inputs; never evaluates acceptance")
    args = parser.parse_args()
    if args.pairs < 1 or args.warmups < 0:
        parser.error("pairs must be positive and warmups non-negative")
    for name in ("baseline_bin", "candidate_bin", "helper_bin", "output", "r_library"):
        setattr(args, name, getattr(args, name).resolve())
    return args


if __name__ == "__main__":
    try:
        sys.exit(run(parse_args()))
    except (OSError, ValueError, RuntimeError, subprocess.CalledProcessError) as error:
        print(error, file=sys.stderr)
        sys.exit(1)
