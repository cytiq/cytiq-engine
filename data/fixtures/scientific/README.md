# Scientific validation inputs

`cymae.lock.json` pins Pennsieve Discover dataset 401, version 1,
DOI [10.26275/qhne-f89h](https://doi.org/10.26275/qhne-f89h), CC BY 4.0.
It lists all 258 published FCS, three population dictionaries and two metadata
files, plus all four published descriptive root assets, including byte sizes,
SHA-256 and S3 object versions (267 files in total). Raw full acquisitions
are fetched into ignored `target/`, never checked in. Published population codes
come from `OmiqFilter` and the `OmiqFilterNames` vocabulary, not inferred gates.

`retention/` contains exact float32 exports of the first contiguous 20,000 events
of one acquisition in each published cohort. `retention.json` records original
file hashes, source row intervals, upstream versions and selected biological
channels. `parity/scientific/make_retention.R` explicitly imports the excerpts and
unmodified PeacoQC 1.20.0 masks (`*.r-mask.json`). These files are not regenerated
by tests or CI.

PeacoQC 1.20.0 `FindThemPeaks` applies a 510-element logical selection to
`dens$x[-1]` (511 elements), recycling its first element and sometimes creating
a spurious right-endpoint peak. For example, Acute2021 Eu151Di events 0–499
produce an extra peak at 2637.249681 with height only 0.001962 of the maximum.
Rust selects the bounded interior grid and does not reproduce this artifact.
`*.r-bounded-mask.json` and `r-population-retention.json` are independent R
references with only this indexing corrected by
`parity/scientific/reference_retention.R`. The unmodified R masks are retained to
make the upstream divergence visible. Neither set is technical artifact truth.
The small excerpts have fewer than 150 bins, so isolation-tree assessment is
explicitly unavailable; full acquisitions are validated separately.

`normal/` contains four unstimulated Ref acquisitions from the published
[CATALYST PBMC example](https://github.com/HelenaLC/CATALYST), commit
`093e4f5755cf208500087f809ed40212761f34cd`. Its GPL (>=2) data source archives and
SHA-256 are recorded in `source.json`. `parity/scientific/export_normal.R`
exports without transforming expression; reread error was exactly zero.
`export.json` and `subjects.csv` describe the complete source export including
stimulated samples, which are excluded from these fixtures. Healthy donor context
is documented in the original
[Bioconductor workshop](https://bioconductor.org/help/course-materials/2017/BioC2017/Day2/Workshops/CyTOF/doc/cytofWorkflow_BioC2017workshop.html).
Four independent subjects give very wide FPR uncertainty. This cohort supports
small-reference stress testing, not clinical calibration.

The seven new FCS reader characterization digests were computed using the
unchanged reader at clean main `6826fc0216b02d128b216a1f5b57f0a33d30608b`;
all 60 prior characterization entries remain unchanged.

`sha256.json` protects fixture inventory and contents. Changing an upstream pin,
data subset or golden reference requires an explicit reviewed import, preserving
old evidence of known divergences. Biological acceptance bounds must follow
measured, dataset-specific evidence; no global removed-fraction bound is used.

`parquet/gzip-size-mismatch.parquet` is a 501-byte independent PyArrow 26.0.0
regression input: four UInt64 values, GZIP dictionary page, with its first page's
declared uncompressed size changed to zero without moving any offsets. Bounded
readers reject the codec before an optionally enabled unbounded decoder can run.

`weak-retention.json` freezes the first measured filtering metrics for a 20x
CD19 attenuation in the labelled pipeline's B/rare events, independently equal
to clean-main binary results. Input/truth hashes and baseline commit/binary are
recorded. It is a post-measurement phenotype perturbation with inherited technical
flags (including irreversible clipping), not independent weak-positive biology.
