#!/usr/bin/env bash
# Restore reviewed R archives and Python wheels without resolving latest deps.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ENV_DIR="$ROOT/parity/.env"
export R_LIBS_USER="$ENV_DIR/Rlib"
expected="${CYTIQ_R_VERSION:?run through just parity-setup}"
found="$(Rscript -e 'cat(as.character(getRversion()))')"
if [[ "$found" != "$expected" ]]; then
  echo "parity requires R $expected; found $found" >&2
  exit 1
fi
python3 "$ROOT/parity/scientific/r_packages.py" install --library "$R_LIBS_USER" --lock "$ROOT/parity/r-packages.lock.json" --cache "$ROOT/target/scientific/r-sources"
uv venv -q --allow-existing "$ENV_DIR/py" --python "$(command -v python3)"
uv pip sync -q --python "$ENV_DIR/py/bin/python" --require-hashes "$ROOT/parity/requirements.lock"
mkdir -p "$ROOT/target/scientific/environment"
Rscript -e 'write.table(installed.packages(lib.loc=Sys.getenv("R_LIBS_USER"))[,c("Package","Version")],stdout(),row.names=FALSE)' > "$ROOT/target/scientific/environment/r-versions.txt"
uv pip freeze --python "$ENV_DIR/py/bin/python" > "$ROOT/target/scientific/environment/python-versions.txt"
