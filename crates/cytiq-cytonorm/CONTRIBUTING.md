# Contributing to cytiq-cytonorm

Workspace rules live in the root CONTRIBUTING.md.

- `manifest.rs`: strict versioned experimental design and relative paths.
- `spline.rs`: independently implemented Fritsch–Carlson interpolation.
- `bundle.rs`: checksum validation, portable models and spline Parquet I/O.
- `runner.rs`: masked cohort loading, balanced sampling (rows drawn by
  `cytiq_flowsom::subsample_rows` with `sample_seed(seed, sample id)`),
  fitting, apply, QC and atomic output publication.

Never copy GPL upstream implementation code. Use the publications, documented
interfaces and black-box R comparisons. Quantiles use `cytiq-density` type 7;
do not substitute smoothing splines for monotone Hermite interpolation.
Keep fixed training scale, marker order, batch membership and transform parameters
in the bundle. Assign clusters before changing any event values. Validation files
must never affect fitted artifacts. Preserve event lineage and all untouched channels.

Bundle, FCS keyword, manifest and Parquet schemas are public versioned contracts.
Reject incomplete mappings and stale checksums. Identity fallback is explicit and
must remain visible in QC. Numerical QC cannot establish biological comparability.

Run `cargo test -p cytiq-cytonorm`, the `cytonorm_cli` tests, `just verify`,
`just parity-cytonorm`, `just parity` and `just parity-real` after numerical changes.
The R generator `parity/cytonorm_numeric.R` owns the committed numerical fixtures;
do not regenerate them to make an implementation change pass.
