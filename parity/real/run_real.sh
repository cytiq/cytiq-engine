#!/usr/bin/env bash
# Validate cytiq on real acquisitions against upstream R tools.
#
# Usage: parity/real/run_real.sh
#   Datasets come from parity/real/datasets.json (R package files installed by
#   parity/setup_env.sh, or URLs), each checked against its SHA-256.
#   Set CYTIQ_REAL_DATA=<dir> to also run every `*.fcs` with `$SPILL` in <dir>
#   (for example internal data that cannot be committed) through the QC and
#   CompensAID chains. Set CYTIQ_CLASSIFY_BUNDLE and CYTIQ_CLASSIFY_GOLDEN to
#   run the `classify` datasets.
# Writes parity/results/metrics_real.json and exits non-zero when a rule in
# parity/real/tolerances.json is violated.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
PARITY="$ROOT/parity"
OUT="${CYTIQ_REAL_OUT:-$ROOT/target/scientific/real}"
RESULTS="${CYTIQ_PARITY_RESULTS:-$ROOT/target/scientific/results}"
mkdir -p "$RESULTS"
PY="$PARITY/.env/py/bin/python"
export R_LIBS_USER="$PARITY/.env/Rlib"
BIN="${CYTIQ_BIN:-$ROOT/target/release/cytiq}"
[[ -n "${CYTIQ_BIN:-}" ]] || (cd "$ROOT" && cargo build --release --quiet)
export CYTIQ_LOG=warn

rm -rf "$OUT"
mkdir -p "$OUT"
# Files over 100 MB: the writer's offset handling (ignored in CI for size). Only reader.rs's
# ignored tests: characterization.rs's `writes_real_data_digests` is a separate tool that needs
# CYTIQ_REAL_DATA and CYTIQ_DIGESTS_OUT, and failed every run without them.
(cd "$ROOT" && cargo test -p cytiq-fcs --release --quiet --test reader -- --ignored)

# --- FlowSOM 68983: compensate, manual gating, logicle, FlowSOM -------------
run_flowsom_chain() {
  local D="$1"
  # FlowJo names compensated channels `Comp-<detector>` and omits the
  # compensation-ref that Gating-ML 2.0 requires (FlowKit rejects the file). The
  # sample is compensated by `cytiq compensate` first, so the dimensions are
  # plain detectors, explicitly `uncompensated`, for both cytiq and FlowKit.
  sed -e 's/name="Comp-/name="/g' \
    -e 's/<gating:dimension/<gating:dimension gating:compensation-ref="uncompensated"/g' \
    "$D/manualGating.xml" > "$D/gates.xml"
  local MARKERS="Pacific Blue-A,APC-A,Alexa Fluor 700-A,APC-Cy7-A,PE-A,PE-Texas Red-A,PE-Cy5-A,PE-Cy7-A"
  "$BIN" compensate --input "$D/sample.fcs" --output "$D/sample.comp.fcs"
  "$BIN" gate --input "$D/sample.comp.fcs" --gating-ml "$D/gates.xml" --output "$D/counts.json" \
    --format json --membership "$D/membership.parquet"
  "$BIN" transform --input "$D/sample.comp.fcs" --output "$D/sample.t.fcs" --method logicle \
    --channels "$MARKERS"
  "$BIN" flowsom --input "$D/sample.t.fcs" --output "$D/flowsom.csv" --channels "$MARKERS" \
    --metaclusters 10
  echo "$MARKERS" > "$D/markers.txt"
}

# --- QC chain (any `$SPILL` file): margins, compensate, logicle, PeacoQC ------
run_qc_chain() {
  local target="$1"
  "$BIN" compensate --input "$target/sample.fcs" --output "$target/sample.comp.fcs"
  local channels
  channels="$("$PY" "$PARITY/real/spill_channels.py" "$target/sample.fcs")"
  echo "$channels" > "$target/channels.txt"
  "$BIN" transform --input "$target/sample.comp.fcs" --output "$target/sample.t.fcs" \
    --method logicle --channels "$channels"
  "$BIN" peacoqc --input "$target/sample.t.fcs" --output "$target/sample.peacoqc.fcs" \
    --channels "$channels"
}

# --- CompensAID chain (any `$SPILL` file): spillqc on the raw file, and with
# --preprocessed on the vignette's own preprocessed file when there is one ----
run_compensaid_chain() {
  local D="$1"
  "$BIN" spillqc --input "$D/sample.fcs" --output "$D/spillqc" --overwrite
  if [[ -f "$D/preprocessed.fcs" ]]; then
    "$BIN" spillqc --input "$D/preprocessed.fcs" --output "$D/spillqc_preprocessed" \
      --preprocessed --overwrite
  fi
}

# --- AutoSpill chain (single-stain controls): spillover matrix and gates ------
run_autospill_chain() {
  local D="$1"
  "$BIN" autospill --controls "$D/controls.csv" --input-dir "$D" \
    --output "$D/autospill_spillover.csv" --matrix-kind spillover --debug-dir "$D/autospill_debug"
}

# --- AutoSpectral chain (spectral single-stain controls, stained samples) ----
run_autospectral_chain() {
  local D="$1"
  # AutoSpectral's control file is cytiq's controls manifest as well
  # (`fluorophore` and `channel` columns, `AF` for the unstained control).
  {
    echo filename
    for fcs in "$D"/sample/*.fcs; do echo "../sample/$(basename "$fcs")"; done
  } > "$D/samples.csv"
  "$BIN" autospectral --controls "$D/control_file.csv" --samples "$D/samples.csv" \
    --input-dir "$D/controls" --output "$D/autospectral"
}

# --- Classify chain: cytiq classify against the native model it was exported from
# CYTIQ_CLASSIFY_BUNDLE is a cytiq-ml bundle; CYTIQ_CLASSIFY_GOLDEN holds its
# native probabilities for the dataset's files (cytiq-ml
# scripts/export_classify_golden.py). Selected classify datasets require both.
run_classify_chain() {
  local D="$1"
  if [[ -z "${CYTIQ_CLASSIFY_BUNDLE:-}" || -z "${CYTIQ_CLASSIFY_GOLDEN:-}" ]]; then
    echo "required classify inputs missing for $(basename "$D"): set CYTIQ_CLASSIFY_BUNDLE and CYTIQ_CLASSIFY_GOLDEN" >&2
    return 1
  fi
  cp -R "$CYTIQ_CLASSIFY_BUNDLE" "$D/bundle"
  cp -R "$CYTIQ_CLASSIFY_GOLDEN" "$D/golden"
  for fcs in "$D"/*.fcs; do
    "$BIN" classify --input "$fcs" --model "$D/bundle" --output "$D/classify/$(basename "$fcs" .fcs)"
  done
}

"$PY" "$PARITY/real/datasets.py" "$OUT" > "$OUT/datasets.tsv"
while IFS=$'\t' read -r name chain; do
  case "$chain" in
    flowsom_gating) run_flowsom_chain "$OUT/$name" ;;
    qc) run_qc_chain "$OUT/$name" ;;
    compensaid) run_compensaid_chain "$OUT/$name" ;;
    autospill) run_autospill_chain "$OUT/$name" ;;
    autospectral) run_autospectral_chain "$OUT/$name" ;;
    classify) run_classify_chain "$OUT/$name" ;;
    *) echo "unknown chain \`$chain\` for $name in datasets.json" >&2; exit 1 ;;
  esac
done < "$OUT/datasets.tsv"
if [[ -n "${CYTIQ_REAL_DATA:-}" ]]; then
  for fcs in "$CYTIQ_REAL_DATA"/*.fcs; do
    [[ -e "$fcs" ]] || continue
    target="$OUT/user_$(basename "$fcs" .fcs)"
    mkdir -p "$target"
    cp "$fcs" "$target/sample.fcs"
    run_qc_chain "$target"
    run_compensaid_chain "$target"
  done
fi

Rscript "$PARITY/real/real.R" "$OUT"
"$PY" "$PARITY/real/compare_real.py" "$OUT" "$RESULTS/metrics_real.json"
