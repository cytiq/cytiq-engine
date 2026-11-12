# Contributing to cytiq-engine

Workspace-wide rules (commands, style, commits, PRs) live in the root
[CONTRIBUTING.md](../../CONTRIBUTING.md). This file covers what is specific to
this crate.

## Layout

- `src/main.rs` — `main` and the `run()` dispatcher: parse the command line,
  start the provenance recorder, hand each `Commands` variant to its module,
  then exit with the outcome's code and write `--outcome`.
- `src/error.rs` — the 0–4 exit contract: `classify` walks an error's
  `source()` chain; `src/error/libraries.rs` holds one exhaustive match per
  library error enum.
- `src/cli.rs` — `Cli`, `Commands` (one newtype variant per command, holding
  its `*Args`), `command_name`, and the `Cli*` value enums with `From` impls
  into library types.
- `src/commands/<name>.rs` — one module per command: its `#[derive(Args)]`
  struct and `run(args, recorder)`, plus its subcommand enum when it has one
  (`MaskCommand`, `FlowSomCommand`, `CytoNormCommand`).
- `src/parsers.rs` — shared argument parsers (`parse_outcome_path`, `parse_threads`,
  `parse_positive_f64`, `parse_finite_f64`, `parse_non_negative_f64`, and
  `parse_channels` for every `--channels` + `--channel` pair, built on
  `cytiq_fcs::parse_channel_list`).
- `src/provenance.rs` — build info, Invocation, the begin macro, and recording under an output lock.
- `src/logging.rs` — the stderr logger; the binary's own modules all log as
  `cytiq`.
- `tests/<command>_cli.rs` — integration tests per subcommand, running the
  `cytiq` binary.

The package is `cytiq-engine`; the binary is `cytiq` (`[[bin]]` in `Cargo.toml`).

## Invariants

- `build.rs` sets `CYTIQ_GIT_COMMIT` (the `CYTIQ_BUILD_COMMIT` environment
  variable, else `.cargo_vcs_info.json` in a packaged crate, else git),
  `CYTIQ_RUSTC_VERSION`, `CYTIQ_TARGET` and `CYTIQ_PROFILE` for provenance
  records. A packaged crate never asks git: its unpacked source may sit inside
  an unrelated repository.

- Keep the CLI thin. A command arm only reads manifests, maps flags into the
  library config, calls `<Crate>Runner::run` and logs. No algorithm, file
  format or output-layout logic here.
- Library crates must not depend on `clap`. Mirror library enums with a local
  `Cli*` enum deriving `ValueEnum` plus `impl From<Cli*> for <LibraryType>`.
  Large commands use a separate `#[derive(Args)]` struct with
  `impl From<Args> for <Config>` (see `CleanArgs` → `CleanConfig`).
- Defaults that the library also needs come from library constants
  (`cytiq_clean::DEFAULT_*`), not duplicated literals.
- Validate values at parse time with `value_parser` (`parse_threads` rejects
  `0`; float parsers reject NaN/inf) rather than inside the runner.
- stdout is for command output only. Everything else goes through `log`
  (`info!` for progress and results, `debug!` for details); the logger writes
  to stderr.
- `Commands` is serialized untagged into provenance, so a command's
  `config` is its `*Args` fields; keep the variant a newtype around them.
  `*Args` structs carry no doc comment: the variant's doc comment is the
  command's `--help` about text.
- Errors propagate as `Box<dyn Error>` from `run()`; `main` prints the error
  and exits with the code `error::classify` assigns (0–4, see the README).
  Channel-list errors are wrapped as `io::ErrorKind::InvalidInput` (code 2).
  Completed QC failures return a typed outcome after the report and provenance
  are written. The corresponding library errors (`QcError::Failed`,
  `PreflightError::Failed`, `SpillQcError::Flagged`) determine reason/code 3.
  Do not `panic!` or call `process::exit` in a command arm.
- `src/error/libraries.rs` matches every variant of every library error
  without a `_` arm, so a new variant does not compile until it is
  classified. When a variant mixes cases that need different codes, split it
  in the library rather than guessing here. Libraries never depend on the
  exit contract; `cytiq-provenance` owns only the type-independent rules
  (`OutcomeReason::of_io`, `of_publish`).

- The `population`/`specimen` families build their own typed outcome
  (`cytiq_specimen::runner::error_reason`), print it as one JSON line after
  argument parsing and return it to `main`, which exits with its code. Package layout,
  validation and publication stay in `cytiq-specimen::runner` and
  `cytiq-population::artifact`.
- `--overwrite` is passed through to the library, which refuses existing
  outputs before any work (`PublishError::Exists`, exit 4). Directory outputs
  use `cytiq_publish::check_output_dir`; their records list
  `DirSnapshot::published`, never `files_under`. `autospill`/`autospread`
  export in the command arm, so the arm checks the output before computing.
  `meta` maps `--output -` to stdout before building `MetaConfig`.

## Adding a subcommand

1. Implement the workflow in a library crate under `crates/` with a
   `<Name>Config`, a `<Name>Runner::run` and a `thiserror` error type; add it
   to the workspace `members`.
2. Add the path dependency to this crate's `Cargo.toml`.
3. Add `src/commands/<name>.rs` with a `#[derive(Debug, Serialize, Args)]`
   `<Name>Args` (field doc comments become `--help` text), reusing the shared
   parsers and the `--threads` / `--debug-dir` / `--channels` + `--channel` /
   `--mask` conventions where they apply, and a `Commands::<Name>(<Name>Args)`
   variant with a doc comment in `src/cli.rs`.
4. Add `run(args, recorder)` in that module and its arm in `main.rs`'s
   dispatcher: `info!("starting <name>: ...")`, build the config, call
   `recorder.snapshot_inputs(role, files)` for each kind of input **before**
   the runner (so inputs are hashed before anything can change them), set
   `set_algorithms(<crate>::UPSTREAM)` and `set_constants` when the crate has
   them, run, log a one-line result summary, and end with `record_provenance`
   listing the output files. When the library resolves inputs itself (globs,
   directories, manifests), expose a `<Name>Runner::input_files`. Add the
   command to `command_name`. The `<Name>Args` fields become the record's
   `config` (`Commands` derives `Serialize`), so new `Cli*` value enums
   derive it too.
5. Classify every variant of the new error enum in `src/error/libraries.rs`
   and register the type in `known`.
6. Add `tests/<name>_cli.rs` covering success, a missing input and at least
   one invalid flag, asserting their exit codes.
7. Document the command in its library crate's `README.md` and in the command
   table of this crate's README. Update the pipeline diagram in the root
   `README.md` and `PIPELINE.md` when stage order or signal-state checks change.

## Where changes belong

- New behaviour, validation of file contents, output formats → the backing
  library crate.
- New flags, flag defaults, `--help` wording, log messages →
  `src/commands/<name>.rs` (a command's about text → `src/cli.rs`).
- Shared fixtures → `data/fixtures/<command>/`.

## Testing

```bash
cargo test -p cytiq-engine
cargo test -p cytiq-engine --test <command>_cli
```

Test files: `autospill_cli.rs`, `autospectral_cli.rs`, `clean_cli.rs`,
`compensate_cli.rs`, `cytoscan_cli.rs`, `exit_codes_cli.rs` (the 0–4 contract
and `--outcome` across command families), `flowsom_cli.rs`, `gate_cli.rs`,
`mask_cli.rs`, `meta_cli.rs`, `parity_smoke.rs` (autospill, AutoSpectral,
PeacoQC, CytoScan and CompensAID against the R outputs in
`data/fixtures/parity`), `peacoqc_cli.rs`, `pipeline_cli.rs` (the whole chain
on `data/fixtures/pipeline` against per-event truth), `plot_cli.rs` (every
plot type, masks, gates checked against `cytiq gate`, classes, batch output,
determinism), `preflight_cli.rs`,
`qc_cli.rs`, `spillqc_cli.rs`, `transform_cli.rs`. `tests/common/mod.rs` holds `assert_provenance`,
`assert_provenance_chain` and `adjusted_rand_index`.
`channels_cli.rs` pins the channel flag errors of every command.
`overwrite_cli.rs` pins the output-conflict contract of every result-writing
command (exit 4, `--overwrite`, `--reuse skip`/`reject`). `threads_cli.rs`
checks that `--threads` never changes outputs. `src/cli_surface.rs` compares
every flag, default and recorded config key with
`tests/fixtures/cli-surface.json`; update that fixture by hand, in the same
change, only for an intended CLI change.
`src/cli.rs` has one unit test, `cli_definition_is_consistent`
(`Cli::command().debug_assert()`); logic worth unit-testing usually belongs in
a library crate. `autospill_cli.rs` and
`autospectral_cli.rs` include `crates/<crate>/tests/support.rs` via
`#[path]`, so changes to those helpers affect both crates' tests.

## Execution and reuse

Command arms accept `Invocation`, snapshot inputs and set algorithm/constants before
`provenance::begin!`. Choose a File or Dir destination matching the library layout.
The macro returns a verified reused outcome or a Run that owns the destination lock;
pass that Run to `record_provenance` (or `record_with_reason` for completed QC).
Never publish before begin or release the lock before the record is durable.
Population/specimen workflows use `artifact::begin_package` and PackageRun::write.
Their callers must snapshot every source before beginning the package.

Use `tests/reuse_cli.rs` for policy, integrity, contention and mtime checks.
`build.rs` fingerprints dirty source including untracked files; keep its Cargo
rerun paths aligned with source/manifests. New global operational flags do not
belong in semantic config. Existing per-command options remain semantic unless
listed in provenance's documented NON_SEMANTIC_OPTIONS.
