# Contributing to cytiq-repair

Workspace guidance lives in [CONTRIBUTING.md](../../CONTRIBUTING.md).

## Layout

- `src/codes.rs`: the stable codes, of repairs and of refusals.
- `src/text.rs`: split TEXT into pairs, marking what can be split in several ways.
- `src/scan.rs`: HEADER and TEXT, read leniently, and the layout they state.
- `src/plan.rs`: the DATA window, what is kept, changed and dropped, the new TEXT.
- `src/repaired.rs`: the repaired file as a reader, without writing it.
- `src/diagnosis.rs`: conforming, repairable or refused; the `--plan` document.
- `src/runner.rs`: write, read back and report; publication is the caller's.
- `fuzz/checks.rs`: the fuzz invariants, replayed by `tests/fuzz_regressions.rs`.

## Invariants

- Lenient parsing lives here and nowhere else. `cytiq-fcs` stays strict, and no
  other command may read a file through this crate.
- A file the strict reader accepts is conforming: it is never repaired.
- A repair is planned only when DATA has exactly one window of `$TOT` events, and
  only when the strict reader accepts what would be written. Anything that can be
  read in two ways is a refusal, never a choice.
- DATA and ANALYSIS are copied byte for byte. HEADER and TEXT are rewritten in
  the same FCS version, with the same delimiter, pairs and order.
- Every byte and keyword left out is listed with its offset, length and SHA-256,
  and never its value. Only blank space is left out without a code.
- A new repair needs a code, a precondition, a file of the smallest size that
  needs it, one that breaks each precondition, and a real file that needs it.
- Codes are never renamed. Documents and records name them.

## Testing

```bash
cargo test -p cytiq-repair
cargo test -p cytiq-engine --test repair_cli
just fuzz cytiq-repair repair_plan 600
```
