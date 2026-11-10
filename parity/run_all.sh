#!/usr/bin/env bash
# End-to-end parity run: generate synthetic data, run cytiq (current tree and
# optionally a baseline ref) and the upstream R tools, then compute metrics.
#
# Usage: parity/run_all.sh [baseline-git-ref]
#   Requires parity/setup_env.sh to have been run once.
#   With a baseline ref (e.g. `main`), it is built in parity/.env/baseline and
#   measured too, producing parity/results/metrics_baseline.json.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PARITY="$ROOT/parity"
DATA="${CYTIQ_PARITY_DATA:-$ROOT/target/scientific/parity-data}"
OUT="${CYTIQ_PARITY_OUT:-$ROOT/target/scientific/parity-out}"
RESULTS="${CYTIQ_PARITY_RESULTS:-$ROOT/target/scientific/results}"
mkdir -p "$RESULTS"
PY="$PARITY/.env/py/bin/python"
export R_LIBS_USER="$PARITY/.env/Rlib"
BASELINE_REF="${1:-}"

"$PY" -m unittest discover -s "$PARITY" -p 'test_classify_safety.py'

rm -rf "$DATA" "$OUT"
"$PY" "$PARITY/gen/generate.py" "$DATA"

"$PARITY/run_rust.sh" "$DATA" "$OUT/rust"
Rscript "$PARITY/run_upstream.R" "$DATA" "$OUT/upstream"
"$PY" "$PARITY/compare.py" "$DATA" "$OUT/rust" "$OUT/upstream" "$RESULTS/metrics.json" >/dev/null

if [[ -n "$BASELINE_REF" ]]; then
  BASE="$PARITY/.env/baseline"
  [[ -d "$BASE" ]] || git -C "$ROOT" worktree add -q "$BASE" "$BASELINE_REF"
  git -C "$BASE" checkout -q --detach "$BASELINE_REF"
  (cd "$BASE" && cargo build --release --quiet)
  CYTIQ_BIN="$BASE/target/release/cytiq" "$PARITY/run_rust.sh" "$DATA" "$OUT/rust_baseline"
  # the AutoSpectral cross-check applies upstream OLS to the baseline's spectra
  cp -R "$OUT/upstream" "$OUT/upstream_baseline"
  PARITY_RUST_OUT="$OUT/rust_baseline" Rscript "$PARITY/run_upstream.R" "$DATA" "$OUT/upstream_baseline" autospectral
  # The baseline is measured, not judged: it may predate checked metrics.
  PARITY_SKIP_TOLERANCES=1 "$PY" "$PARITY/compare.py" "$DATA" "$OUT/rust_baseline" "$OUT/upstream_baseline" \
    "$RESULTS/metrics_baseline.json" >/dev/null
fi

echo "metrics written to $RESULTS/"
