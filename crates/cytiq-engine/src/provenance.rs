use std::collections::BTreeMap;
use std::env;
use std::io::Read as _;
use std::num::NonZeroUsize;
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};

use cytiq_fcs::{FcsReadLimits, FcsReader};
use cytiq_provenance::{
    BuildInfo, RunRecorder,
    outcome::{CommandOutcome, Execution, OutcomeReason},
    reuse::{self, Decision, Destination, Prior, ReusePolicy},
};
use cytiq_publish::{OutputLock, PublishError};

pub(crate) type CommandResult = Result<Option<CommandOutcome>, Box<dyn std::error::Error>>;

pub(crate) fn build_info() -> BuildInfo {
    BuildInfo {
        git_commit: env!("CYTIQ_GIT_COMMIT").to_owned(),
        rustc: env!("CYTIQ_RUSTC_VERSION").to_owned(),
        target: env!("CYTIQ_TARGET").to_owned(),
        profile: env!("CYTIQ_PROFILE").to_owned(),
    }
}

pub(crate) struct Invocation {
    pub(crate) recorder: RunRecorder,
    pub(crate) policy: ReusePolicy,
}
pub(crate) struct Run {
    recorder: RunRecorder,
    _lock: Option<OutputLock>,
}
pub(crate) enum Begun {
    Reused(CommandOutcome),
    Compute(Box<Run>),
}

impl Deref for Invocation {
    type Target = RunRecorder;
    fn deref(&self) -> &RunRecorder {
        &self.recorder
    }
}
impl DerefMut for Invocation {
    fn deref_mut(&mut self) -> &mut RunRecorder {
        &mut self.recorder
    }
}
impl Invocation {
    pub(crate) fn begin(
        mut self,
        destination: &Destination,
    ) -> Result<Begun, Box<dyn std::error::Error>> {
        record_quarantined_keywords(&mut self.recorder);
        let expected = self.recorder.snapshot_record();
        if self.policy == ReusePolicy::Reject && option_enabled(&expected.config, "overwrite") {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "--reuse reject cannot be combined with --overwrite",
            )
            .into());
        }
        let lock = match OutputLock::acquire(destination.path()) {
            Ok(lock) => Some(lock),
            Err(PublishError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::Unsupported
                    && self.policy == ReusePolicy::Recompute =>
            {
                log::warn!("filesystem does not support output locking: {source}");
                None
            }
            Err(error) => return Err(error.into()),
        };
        if let Some(lock) = &lock {
            cytiq_publish::recovery::recover(lock)?;
        }
        if self.policy != ReusePolicy::Recompute {
            let mut prior = reuse::assess(destination, &expected);
            if option_enabled(&expected.config, "debug_dir") && matches!(prior, Prior::Match(_)) {
                prior = Prior::Stale {
                    why: "debug artifacts require a fresh run".into(),
                };
            }
            if let Prior::Stale { why } = &prior {
                log::info!("prior output is stale: {why}");
            }
            match reuse::decide(self.policy, &prior) {
                Decision::Reuse => {
                    let Prior::Match(record) = prior else {
                        unreachable!("reuse requires a match");
                    };
                    self.recorder.verify_inputs()?;
                    log::info!("reusing verified output {}", destination.path().display());
                    let mut outcome = CommandOutcome::new(
                        &record.tool,
                        true,
                        record.reason.expect("assessed reason"),
                        String::new(),
                    );
                    outcome.artifacts.clone_from(&record.outputs);
                    outcome.execution = Some(Execution {
                        fingerprint: record.execution_fingerprint.expect("assessed fingerprint"),
                        reused: true,
                    });
                    return Ok(Begun::Reused(outcome));
                }
                Decision::Conflict => {
                    return Err(PublishError::Exists {
                        path: destination.path().to_owned(),
                    }
                    .into());
                }
                Decision::Compute => {}
            }
        }
        Ok(Begun::Compute(Box::new(Run {
            recorder: self.recorder,
            _lock: lock,
        })))
    }
}

/// Record the keywords each FCS input repeats, by the input's digest: nothing
/// reads their values. A file that does not parse is the command's to refuse.
fn record_quarantined_keywords(recorder: &mut RunRecorder) {
    let found: BTreeMap<String, Vec<String>> = recorder
        .inputs()
        .filter_map(|(path, record)| {
            let keywords = quarantined_keywords(path)?;
            (!keywords.is_empty()).then(|| (record.sha256.clone(), keywords))
        })
        .collect();
    if !found.is_empty() {
        for (sha256, keywords) in &found {
            log::warn!(
                "input {sha256} repeats keywords, which are ignored: {}",
                keywords.join(", ")
            );
        }
        recorder.set_input_facts("quarantined_keywords", serde_json::json!(found));
    }
}

fn quarantined_keywords(path: &Path) -> Option<Vec<String>> {
    let mut magic = [0_u8; 3];
    std::fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut magic))
        .ok()?;
    if &magic != b"FCS" {
        return None;
    }
    let stream = FcsReader::open(path)
        .and_then(|reader| reader.stream(NonZeroUsize::MIN, FcsReadLimits::default()))
        .ok()?;
    let text = &stream.metadata.text;
    Some(text.quarantined().map(str::to_owned).collect())
}

fn option_enabled(value: &serde_json::Value, name: &str) -> bool {
    match value {
        serde_json::Value::Object(map) => map.iter().any(|(key, value)| {
            (key == name && !value.is_null() && value != &serde_json::Value::Bool(false))
                || option_enabled(value, name)
        }),
        serde_json::Value::Array(items) => items.iter().any(|v| option_enabled(v, name)),
        _ => false,
    }
}

// Keep the command arms linear while returning a reused outcome to the dispatcher.
macro_rules! begin {
    ($invocation:expr, $destination:expr) => {
        match $invocation.begin(&$destination)? {
            $crate::provenance::Begun::Compute(run) => *run,
            $crate::provenance::Begun::Reused(outcome) => return Ok(Some(outcome)),
        }
    };
}
pub(crate) use begin;

pub(crate) fn record_provenance(
    run: Run,
    outputs: &[PathBuf],
    record_path: &Path,
) -> CommandResult {
    record_with_reason(run, outputs, record_path, OutcomeReason::Success)
}

pub(crate) fn record_with_reason(
    run: Run,
    outputs: &[PathBuf],
    record_path: &Path,
    reason: OutcomeReason,
) -> CommandResult {
    let record = run.recorder.finish(outputs, record_path, reason)?;
    let mut outcome = CommandOutcome::new(&record.tool, true, reason, String::new());
    outcome.artifacts = record.outputs;
    outcome.execution = Some(Execution {
        fingerprint: record.execution_fingerprint.expect("v3 fingerprint"),
        reused: false,
    });
    Ok(Some(outcome))
}
