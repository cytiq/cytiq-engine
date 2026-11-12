#!/usr/bin/env bash
# Run every cytiq subcommand on the synthetic parity datasets.
# Usage: parity/run_rust.sh <data_dir> <out_dir>
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DATA="${1:-$ROOT/parity/.data}"
OUT="${2:-$ROOT/parity/.out/rust}"
# CYTIQ_BIN lets the harness run another build (e.g. the pre-fix baseline).
if [[ -n "${CYTIQ_BIN:-}" ]]; then
  BIN="$CYTIQ_BIN"
else
  BIN="$ROOT/target/release/cytiq"
  (cd "$ROOT" && cargo build --release --quiet)
fi
mkdir -p "$OUT"

# BSD time (macOS) reports resources with -l, GNU time (Linux CI) with -v.
if /usr/bin/time -l true >/dev/null 2>&1; then
  TIME_FLAG=-l
else
  TIME_FLAG=-v
fi

timed() {
  local name="$1"
  shift
  /usr/bin/time "$TIME_FLAG" "$@" 2>"$OUT/$name.time" >"$OUT/$name.log" || {
    cat "$OUT/$name.time" >&2
    exit 1
  }
}

mkdir -p "$OUT/transform"
for method in arcsinh logicle hyperlog biexponential; do
  timed "transform_$method" "$BIN" transform -i "$DATA/transform/grid.fcs" \
    -o "$OUT/transform/$method.fcs" --method "$method" --channels X --overwrite
done

timed autospill "$BIN" autospill --controls "$DATA/autospill/controls.csv" \
  --input-dir "$DATA/autospill" --output "$OUT/autospill_spillover.csv" \
  --matrix-kind spillover --debug-dir "$OUT/autospill_debug" --overwrite
timed autospread "$BIN" autospread --controls "$DATA/autospill/controls.csv" \
  --input-dir "$DATA/autospill" --output "$OUT/autospread.csv" --overwrite

rm -rf "$OUT/autospectral"
timed autospectral "$BIN" autospectral --controls "$DATA/autospectral/controls.csv" \
  --samples "$DATA/autospectral/samples.csv" --input-dir "$DATA/autospectral" \
  --output "$OUT/autospectral"

for method in all mad it; do
  timed "peacoqc_$method" "$BIN" peacoqc --input "$DATA/peacoqc/sample.fcs" \
    --output "$OUT/peacoqc_$method.fcs" --channels "$(cat "$DATA/peacoqc/channels.txt")" \
    --method "$method" --debug-dir "$OUT/peacoqc_${method}_debug" --overwrite
done

timed spillqc "$BIN" spillqc --input "$DATA/spillqc" --output "$OUT/spillqc" --overwrite

for dataset in flowsom flowsom_hard; do
  timed "$dataset" "$BIN" flowsom --input "$DATA/$dataset/sample.fcs" \
    --output "$OUT/$dataset.csv" --channels "$(cat "$DATA/$dataset/channels.txt")" \
    --debug-dir "$OUT/${dataset}_debug" --overwrite
done

for dataset in cytoscan_exact cytoscan cytoscan_ref100; do
  [[ -d "$DATA/$dataset" ]] || continue
  for method in summary quantiles emd binning fingerprint; do
    # The debug directory holds the reference features run_upstream.R fits
    # Mclust on.
    timed "${dataset}_$method" "$BIN" cytoscan --input "$DATA/$dataset/test" \
      --reference "$DATA/$dataset/reference" --output "$OUT/$dataset/$method" \
      --channels "$(cat "$DATA/$dataset/channels.txt")" --method "$method" --flag-method both \
      --debug-dir "$OUT/${dataset}_debug/$method" --overwrite
  done
done

timed gating "$BIN" gate --input "$DATA/gating/sample.fcs" \
  --gating-ml "$DATA/gating/gates.xml" --output "$OUT/gating.json" --format json --overwrite
QUADRANT="$ROOT/data/fixtures/gating/quadrant"
timed gating_quadrant "$BIN" gate --input "$QUADRANT/sample.fcs" \
  --gating-ml "$QUADRANT/gates.xml" --output "$OUT/gating_quadrant.json" --format json --overwrite

# End-to-end golden pipeline (parity/gen/pipeline.py): the documented chain.
FLUOR=FL1-A,FL2-A,FL3-A,FL4-A,FL5-A
for dataset in pipeline pipeline_beads; do
  [[ -d "$DATA/$dataset" ]] || continue
  P="$OUT/$dataset"
  rm -rf "$P" && mkdir -p "$P"
  timed "${dataset}_preflight" "$BIN" preflight --manifest "$DATA/$dataset/run.yaml" \
    --output "$P/preflight.json"
  timed "${dataset}_compensate" "$BIN" compensate --input "$DATA/$dataset/sample.fcs" \
    --output "$P/s.comp.fcs"
  timed "${dataset}_transform" "$BIN" transform --input "$P/s.comp.fcs" --output "$P/s.t.fcs"
  timed "${dataset}_peacoqc" "$BIN" peacoqc --input "$P/s.t.fcs" --output "$P/s.peacoqc.fcs" \
    --channels "$FLUOR"
  timed "${dataset}_clean" "$BIN" clean --input "$DATA/$dataset/sample.fcs" --output "$P/clean"
  timed "${dataset}_mask" "$BIN" mask combine --mask "$P/s.comp.masks.parquet" \
    --mask "$P/s.peacoqc.masks.parquet" --mask "$P/clean/masks.parquet" --output "$P/analysis.parquet"
  timed "${dataset}_gate" "$BIN" gate --input "$P/s.t.fcs" --gating-ml "$DATA/$dataset/gates.xml" \
    --output "$P/counts.json" --format json --mask "$P/analysis.parquet" \
    --membership "$P/membership.parquet"
  timed "${dataset}_flowsom" "$BIN" flowsom --input "$P/s.t.fcs" --output "$P/flowsom.csv" \
    --channels "$FLUOR" --mask "$P/analysis.parquet" --metaclusters 7
done

echo "rust outputs in $OUT"
