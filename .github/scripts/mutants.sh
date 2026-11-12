#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname -- "${BASH_SOURCE[0]}")/../.."
if [[ "$(cargo mutants --version)" != 'cargo-mutants 27.1.0' ]]; then
    echo 'Expected cargo-mutants 27.1.0; see CONTRIBUTING.md for installation.' >&2
    exit 1
fi

crates=(cytiq-engine cytiq-clean cytiq-flowsom cytiq-peacoqc
        cytiq-spillqc cytiq-gating cytiq-fcs cytiq-cytonorm cytiq-classify
        cytiq-population cytiq-specimen)
if [[ "${1:-all}" != all ]]; then
    selected="$1"
    found=false
    for crate in "${crates[@]}"; do
        if [[ "$crate" == "$selected" ]]; then found=true; fi
    done
    if [[ "$found" != true ]]; then
        echo "Unknown mutation target: $selected. Expected all or: ${crates[*]}" >&2
        exit 2
    fi
    crates=("$selected")
fi

status=0
for crate in "${crates[@]}"; do
    directory="target/mutation/$crate"
    mkdir -p "$directory"
    # Never empty: bash 3.2 (macOS) rejects "${empty[@]}" under `set -u`.
    target=(--package "$crate")
    if [[ "$crate" == cytiq-fcs ]]; then target+=(--features inputs); fi
    cargo mutants "${target[@]}" --list --json > "$directory/selected.json"
    python3 .github/scripts/mutants_selection.py "$directory/selected.json"
    # Do not skip baseline tests or mutate the working tree.
    cargo mutants "${target[@]}" --jobs 1 --jobserver-tasks 4 --output "$directory" || status=1
done
exit "$status"
