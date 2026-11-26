# cytiq-repair

Explicit repair of FCS files that instruments wrote against the standard.

Every other cytiq command reads FCS strictly and refuses a file that deviates,
because a reader that guesses can read other values than were stored. Some
instruments write such files. This crate names the deviations, and says whether
DATA can still be read in exactly one way.

```bash
cytiq repair --input instrument.fcs --plan
cytiq repair --input instrument.fcs --output repaired.fcs --allow text_trailing_padding
```

Nothing is repaired without `--allow CODE`, which names one repair and can be
repeated. A file that needs a repair that was not allowed is refused (exit 2) and
the message lists what it needs. A conforming file is refused too: there is
nothing to repair, so a second repair never happens.

The repaired file has the same FCS version, the same delimiter, and the same
keywords in the same order, except those a repair drops. Its DATA and ANALYSIS
are the source's bytes. HEADER, the offset keywords and the CRC (eight `0`, FCS
3.1, 3.5: not computed) are new, and two keywords are added:

- `CYTIQ_ACQUISITION`: the SHA-256 of the file the instrument wrote. Every stage
  carries it, so masks, labels and tables name that file.
- `CYTIQ_REPAIR`: `{"codes":[…],"version":1}`.

Such a file stands for its raw acquisition in every cytiq command. Beside it are
`<output>.repair.json` (`cytiq.repair.report.v1`: what was changed, and the
offset, length and SHA-256 of everything dropped; no path and no keyword value)
and the provenance record. The three are published together.

Before that, the staged file is read back: the strict reader accepts it, it needs
no repair, its DATA hashes to the source's, and the source did not change. A file
that fails one of these is not published (exit 1).

`--plan` prints the diagnosis as JSON (`cytiq.repair.plan.v1`) and writes
nothing: `conforming`, `repairable` with the repairs it needs, or `refused` with
the reason. It names no path and no keyword value. It exits 0 for each of the
three, 4 when the file is missing.

## Repairs

| Code | The file | Precondition |
| --- | --- | --- |
| `text_trailing_padding` | Spaces or NULs after the last delimiter of TEXT | Nothing else follows it |
| `text_overlaps_data` | TEXT ends at the byte where DATA begins | HEADER states both |
| `text_empty_values` | A doubled delimiter where a keyword is expected: an empty value, or an escape in the name | Every reading ends at the same byte; the pairs they disagree on are dropped, and no keyword the layout needs is among them |
| `data_end_exclusive` | DATA is declared one byte longer than `$TOT` events, up to the length of the file | Starting one byte later would end outside the file |
| `supplemental_text_inside_text` | The supplemental TEXT offsets are those of TEXT, or of one delimiter | It holds no keyword that TEXT does not |
| `analysis_offsets_negative` | HEADER ends ANALYSIS at `-1` | TEXT says there is none |
| `undeclared_bytes` | Bytes outside every declared segment hold something | They are dropped, with the vendor keywords that hold their offsets |

Every repair also requires that `$TOT` times the width of an event is the length
of DATA, that HEADER and TEXT agree on where it is (or HEADER holds zeros), and
that the strict reader accepts the repaired file.

## Refusals

`not_fcs`, `unsupported_version` (FCS 2.0 among them), `header_unreadable`,
`text_out_of_file`, `text_unterminated`, `ambiguous_text`, `delimiter`,
`repeated_keyword`, `missing_layout`, `unsupported_layout`, `multiple_data_sets`,
`supplemental_text`, `other_segments`, `ambiguous_data_offsets`,
`data_length_mismatch`, `ambiguous_analysis`, `segments_overlap`, `derived_file`
and `still_refused`.

## Public API

| Item | Purpose |
| --- | --- |
| `diagnose(reader)` | `Diagnosis::{Conforming, Repairable, Refused}` of anything that reads and seeks |
| `Plan` | `codes`, `findings`, `changes`, `dropped`, the `DataWindow`; `repaired(source, acquisition)` reads the repaired file without writing it |
| `PlanDocument::of(path)` | The document `--plan` prints |
| `RepairRunner::prepare`, `run`, `RepairConfig`, `RepairReport`, `report_path` | Stage, or publish, the repaired file and its report |
| `RepairCode`, `RefusalCode`, `Refusal` | Stable codes |
| `REPAIR_KEYWORD`, `REPAIR_VERSION` | `CYTIQ_REPAIR` and its version |

## What real files need

| Instrument | Repairs |
| --- | --- |
| Sony ID7000 | `text_trailing_padding` |
| NovoCyte Opteon | `text_trailing_padding`, `supplemental_text_inside_text` and, when the undeclared bytes after DATA hold something, `undeclared_bytes` |
| Attune Xenith | `data_end_exclusive` |
| Attune NxT | `text_trailing_padding` |
| Attune | `text_trailing_padding`, `text_empty_values` |
| BD FACSAria II | `analysis_offsets_negative` |
| Accuri C6 | `data_end_exclusive`, `supplemental_text_inside_text` |
| CyTOF 6 | `text_overlaps_data`, `text_trailing_padding`, `data_end_exclusive` |
| CyAn | refused, `ambiguous_data_offsets`: HEADER and TEXT put DATA 46 bytes apart |
| Guava easyCyte | refused, `text_unterminated`: unescaped delimiters in TEXT; HEADER and TEXT put DATA 330 bytes apart |

These are the files of the Phase 1 corpus, not every file of those instruments.
A repair says how a file is read, not that its measurements are right.

## Dependencies

`cytiq-fcs`, `cytiq-provenance`, `cytiq-publish`, `serde` / `serde_json`, `sha2`, `log`,
`thiserror`.

## Tests

```bash
cargo test -p cytiq-repair
cargo test -p cytiq-engine --test repair_cli
```
