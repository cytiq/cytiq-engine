//! Mask metrics and the policy rules evaluated on them.

use cytiq_manifest::{AssessedOf, FilterPolicy, QcPolicy, Severity};
use cytiq_mask::EventMask;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Pass,
    Warn,
    Fail,
}

impl From<Severity> for CheckStatus {
    fn from(severity: Severity) -> Self {
        match severity {
            Severity::Fail => Self::Fail,
            Severity::Warn => Self::Warn,
        }
    }
}

/// One QC finding. `value` and `limit` hold the measured number and the
/// policy bound when the check compares them.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct QcCheck {
    /// `sample S01` or `filter peacoqc_pass`.
    pub scope: String,
    pub check: &'static str,
    pub status: CheckStatus,
    pub message: String,
    pub value: Option<f64>,
    pub limit: Option<f64>,
}

impl QcCheck {
    pub(crate) fn new(
        scope: &str,
        check: &'static str,
        status: CheckStatus,
        message: String,
    ) -> Self {
        Self {
            scope: scope.to_owned(),
            check,
            status,
            message,
            value: None,
            limit: None,
        }
    }

    const fn compared(mut self, value: f64, limit: f64) -> Self {
        self.value = Some(value);
        self.limit = Some(limit);
        self
    }
}

/// What one filter did, over every mask column that carries it: `live`, and
/// `2.live` when `cytiq mask combine` met the name twice.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FilterMetrics {
    pub name: String,
    pub columns: Vec<String>,
    /// Events at least one column assessed (non-null).
    pub assessed: usize,
    /// `assessed` over the mask's rows, every raw event.
    pub assessed_fraction: f64,
    /// Events no other filter removed.
    pub eligible: usize,
    /// Share of `eligible` the filter assessed; `None` when other filters
    /// removed every event.
    pub eligible_assessed_fraction: Option<f64>,
    /// Events at least one column failed.
    pub removed: usize,
    /// `removed / assessed`; `None` when the filter assessed nothing.
    pub removed_fraction: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct QcMetrics {
    /// Events in the raw sample file.
    pub events: usize,
    /// Raw events the mask keeps.
    pub kept: usize,
    pub removed_fraction: f64,
    pub filters: Vec<FilterMetrics>,
}

impl QcMetrics {
    /// Metrics of `mask` for a raw file of `events` events, `kept` of which
    /// the mask keeps.
    #[must_use]
    pub fn of(mask: &EventMask, events: usize, kept: usize) -> Self {
        Self {
            events,
            kept,
            removed_fraction: fraction(events - kept, events).unwrap_or(0.0),
            filters: filter_metrics(mask),
        }
    }
}

/// `2.margin_pass` → `margin_pass`: the prefix `cytiq mask combine` adds to a
/// repeated filter name.
fn base_name(mut column: &str) -> &str {
    while let Some((index, name)) = column.split_once('.') {
        if index.is_empty() || !index.bytes().all(|b| b.is_ascii_digit()) {
            break;
        }
        column = name;
    }
    column
}

#[allow(clippy::cast_precision_loss)]
fn fraction(part: usize, whole: usize) -> Option<f64> {
    (whole > 0).then(|| part as f64 / whole as f64)
}

fn filter_metrics(mask: &EventMask) -> Vec<FilterMetrics> {
    let mut names = Vec::<&str>::new();
    for (column, _) in &mask.filters {
        let name = base_name(column);
        if !names.contains(&name) {
            names.push(name);
        }
    }
    // Per filter and row: assessed by any of its columns, removed by any.
    let flags = names
        .into_iter()
        .map(|name| {
            let columns = mask
                .filters
                .iter()
                .filter(|(column, _)| base_name(column) == name)
                .collect::<Vec<_>>();
            let (assessed, removed) = (0..mask.len())
                .map(|row| {
                    let mut values = columns.iter().map(|(_, values)| values[row]);
                    let assessed = values.clone().any(|value| value.is_some());
                    (assessed, values.any(|value| value == Some(false)))
                })
                .unzip::<_, _, Vec<_>, Vec<_>>();
            (name, columns, assessed, removed)
        })
        .collect::<Vec<_>>();
    // A row is eligible for a filter when no other filter removed it.
    let mut removers = vec![0_usize; mask.len()];
    for (_, _, _, removed) in &flags {
        for (count, removed) in removers.iter_mut().zip(removed) {
            *count += usize::from(*removed);
        }
    }
    flags
        .into_iter()
        .map(|(name, columns, assessed, removed)| {
            let (mut eligible, mut eligible_assessed) = (0, 0);
            for (row, others) in removers.iter().enumerate() {
                if *others == usize::from(removed[row]) {
                    eligible += 1;
                    eligible_assessed += usize::from(assessed[row]);
                }
            }
            let assessed = assessed.iter().filter(|assessed| **assessed).count();
            let removed = removed.iter().filter(|removed| **removed).count();
            FilterMetrics {
                name: name.to_owned(),
                columns: columns.iter().map(|(column, _)| column.clone()).collect(),
                assessed,
                assessed_fraction: fraction(assessed, mask.len()).unwrap_or(0.0),
                eligible,
                eligible_assessed_fraction: fraction(eligible_assessed, eligible),
                removed,
                removed_fraction: fraction(removed, assessed),
            }
        })
        .collect()
}

fn percent(value: f64) -> String {
    format!("{:.1}%", value * 100.0)
}

/// Every rule of `policy` checked against `metrics`.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn evaluate(sample_scope: &str, policy: &QcPolicy, metrics: &QcMetrics) -> Vec<QcCheck> {
    let mut checks = Vec::new();
    let at_least = |check, what: &str, value: usize, limit: u64| {
        let status = if value as u64 >= limit {
            CheckStatus::Pass
        } else {
            CheckStatus::Fail
        };
        QcCheck::new(
            sample_scope,
            check,
            status,
            format!("{value} {what} (minimum {limit})"),
        )
        .compared(value as f64, limit as f64)
    };
    if let Some(limit) = policy.min_events {
        checks.push(at_least(
            "min_events",
            "raw event(s)",
            metrics.events,
            limit,
        ));
    }
    if let Some(limit) = policy.min_kept_events {
        checks.push(at_least(
            "min_kept_events",
            "kept event(s)",
            metrics.kept,
            limit,
        ));
    }
    if let Some(limit) = policy.max_removed_fraction {
        let value = metrics.removed_fraction;
        checks.push(
            QcCheck::new(
                sample_scope,
                "max_removed_fraction",
                if value <= limit {
                    CheckStatus::Pass
                } else {
                    CheckStatus::Fail
                },
                format!(
                    "{} of raw events removed (maximum {})",
                    percent(value),
                    percent(limit)
                ),
            )
            .compared(value, limit),
        );
    }
    for (name, rule) in &policy.filters {
        checks.extend(evaluate_filter(name, rule, metrics));
    }
    checks
}

/// Release requires an effective panel policy and every mandatory assessment,
/// independently of the severity used for exploratory reporting.
#[must_use]
pub fn evaluate_release(
    scope: &str,
    policy: Option<&QcPolicy>,
    metrics: &QcMetrics,
) -> Vec<QcCheck> {
    let Some(policy) = policy.filter(|policy| policy.has_rules()) else {
        return vec![QcCheck::new(
            scope,
            "policy",
            CheckStatus::Fail,
            "release requires an explicit nonempty panel QC policy".to_owned(),
        )];
    };
    let mut checks = evaluate(scope, policy, metrics);
    for (name, rule) in &policy.filters {
        if rule.required
            && !metrics
                .filters
                .iter()
                .any(|filter| filter.name == *name && filter.assessed > 0)
        {
            checks.push(QcCheck::new(
                &format!("filter {name}"),
                "required_assessment",
                CheckStatus::Fail,
                format!("mandatory filter `{name}` was not assessed; release is blocked"),
            ));
        }
    }
    checks
}

fn evaluate_filter(name: &str, rule: &FilterPolicy, metrics: &QcMetrics) -> Vec<QcCheck> {
    let mut checks = Vec::new();
    let scope = format!("filter {name}");
    let violated = CheckStatus::from(rule.severity);
    let optional = if rule.required { "" } else { " (optional)" };
    let missing = if rule.required {
        violated
    } else {
        CheckStatus::Warn
    };
    let Some(filter) = metrics.filters.iter().find(|filter| filter.name == name) else {
        checks.push(QcCheck::new(
            &scope,
            "filter_assessed",
            missing,
            format!("`{name}` is not in the mask{optional}"),
        ));
        return checks;
    };
    if filter.assessed == 0 {
        checks.push(QcCheck::new(
            &scope,
            "filter_assessed",
            missing,
            format!("`{name}` assessed no event{optional}"),
        ));
    } else if rule.required {
        checks.push(QcCheck::new(
            &scope,
            "filter_assessed",
            CheckStatus::Pass,
            format!("`{name}` ran"),
        ));
    }
    // A filter in the mask must cover its share of events even when it is
    // optional: `required` only asks that it ran.
    if let Some(limit) = rule.min_assessed_fraction {
        let (value, of) = match rule.assessed_of {
            AssessedOf::Raw => (Some(filter.assessed_fraction), "raw events"),
            AssessedOf::Eligible => (
                filter.eligible_assessed_fraction,
                "the events no other filter removed",
            ),
        };
        checks.push(match value {
            Some(value) => QcCheck::new(
                &scope,
                "filter_assessed_fraction",
                if value >= limit {
                    CheckStatus::Pass
                } else {
                    violated
                },
                format!(
                    "`{name}` assessed {} of {of} (minimum {})",
                    percent(value),
                    percent(limit)
                ),
            )
            .compared(value, limit),
            None => QcCheck::new(
                &scope,
                "filter_assessed_fraction",
                CheckStatus::Pass,
                format!("other filters removed every event, so none was left for `{name}`"),
            ),
        });
    }
    if let (Some(limit), Some(removed_fraction)) =
        (rule.max_removed_fraction, filter.removed_fraction)
    {
        checks.push(
            QcCheck::new(
                &scope,
                "filter_removed_fraction",
                if removed_fraction <= limit {
                    CheckStatus::Pass
                } else {
                    violated
                },
                format!(
                    "`{name}` removed {} of the events it assessed (maximum {})",
                    percent(removed_fraction),
                    percent(limit)
                ),
            )
            .compared(removed_fraction, limit),
        );
    }
    checks
}

#[cfg(test)]
mod tests {
    #[test]
    fn nested_combine_prefixes_preserve_assessment_and_union_of_failures() {
        let mask = super::EventMask::new("a".repeat(64), vec![true, false, false], None)
            .with_filter("live", vec![None, None, Some(false)])
            .with_filter("2.live", vec![None, Some(false), None])
            .with_filter("2.2.live", vec![Some(true), None, None]);
        let metrics = super::QcMetrics::of(&mask, 3, 1);
        assert_eq!(metrics.filters.len(), 1);
        assert_eq!(metrics.filters[0].name, "live");
        assert_eq!(metrics.filters[0].assessed, 3);
        assert_eq!(metrics.filters[0].removed, 2);
    }
    #[test]
    fn release_requires_effective_policy_and_mandatory_assessment_even_when_severity_warns() {
        let metrics = super::QcMetrics::of(
            &super::EventMask::new("a".repeat(64), vec![true; 3], None)
                .with_filter("live", vec![None; 3]),
            3,
            3,
        );
        for policy in [None, Some(cytiq_manifest::QcPolicy::default())] {
            assert!(
                super::evaluate_release("sample", policy.as_ref(), &metrics)
                    .iter()
                    .any(|c| c.status == super::CheckStatus::Fail)
            );
        }
        let policy = cytiq_manifest::QcPolicy {
            filters: std::collections::BTreeMap::from([(
                "live".into(),
                cytiq_manifest::FilterPolicy {
                    required: true,
                    severity: cytiq_manifest::Severity::Warn,
                    ..Default::default()
                },
            )]),
            ..Default::default()
        };
        assert!(
            super::evaluate("sample", &policy, &metrics)
                .iter()
                .all(|c| c.status != super::CheckStatus::Fail)
        );
        assert!(
            super::evaluate_release("sample", Some(&policy), &metrics)
                .iter()
                .any(|c| c.status == super::CheckStatus::Fail)
        );
        let optional = cytiq_manifest::QcPolicy {
            filters: std::collections::BTreeMap::from([(
                "live".into(),
                cytiq_manifest::FilterPolicy::default(),
            )]),
            ..Default::default()
        };
        assert_eq!(
            super::evaluate("sample", &optional, &metrics)[0].status,
            super::CheckStatus::Warn
        );
    }
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn check_status_preserves_wire_values_and_severity_order() {
        let statuses = [CheckStatus::Pass, CheckStatus::Warn, CheckStatus::Fail];
        for (status, value) in statuses.into_iter().zip(["pass", "warn", "fail"]) {
            let json = format!("\"{value}\"");
            assert_eq!(serde_json::to_string(&status).unwrap(), json);
            assert_eq!(serde_json::from_str::<CheckStatus>(&json).unwrap(), status);
        }
        assert!(statuses.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(CheckStatus::from(Severity::Warn), CheckStatus::Warn);
        assert_eq!(CheckStatus::from(Severity::Fail), CheckStatus::Fail);
    }

    const RAW: &str = "1111111111111111111111111111111111111111111111111111111111111111";

    /// Ten raw events: margins removed 1, `PeacoQC` (as `2.margin_pass` and
    /// `peacoqc_pass`) removed 3 of the 9 it assessed, `live` never ran.
    fn mask() -> EventMask {
        let flags = |removed: &[usize], skipped: &[usize]| {
            (0..10)
                .map(|row| (!skipped.contains(&row)).then_some(!removed.contains(&row)))
                .collect::<Vec<_>>()
        };
        let keep = (0..10).map(|row| ![0, 4, 5, 6].contains(&row)).collect();
        EventMask::new(RAW, keep, None)
            .with_filter("margin_pass", flags(&[0], &[]))
            .with_filter("2.margin_pass", flags(&[], &(0..10).collect::<Vec<_>>()))
            .with_filter("peacoqc_pass", flags(&[4, 5, 6], &[0]))
            .with_filter("live", vec![None; 10])
    }

    fn policy(filters: &[(&str, FilterPolicy)]) -> QcPolicy {
        QcPolicy {
            min_events: Some(10),
            min_kept_events: Some(7),
            max_removed_fraction: Some(0.3),
            filters: filters
                .iter()
                .map(|(name, rule)| ((*name).to_owned(), rule.clone()))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    #[test]
    fn groups_prefixed_columns_into_one_filter() {
        let metrics = QcMetrics::of(&mask(), 10, 6);
        let margin = &metrics.filters[0];
        assert_eq!(margin.name, "margin_pass");
        assert_eq!(margin.columns, ["margin_pass", "2.margin_pass"]);
        assert_eq!((margin.assessed, margin.removed), (10, 1));
        let peacoqc = &metrics.filters[1];
        assert_eq!((peacoqc.assessed, peacoqc.removed), (9, 3));
        assert!((peacoqc.assessed_fraction - 0.9).abs() < 1e-12);
        assert_eq!(metrics.filters[2].removed_fraction, None);
        assert!((metrics.removed_fraction - 0.4).abs() < 1e-12);
    }

    #[test]
    fn eligible_events_are_those_no_other_filter_removed() {
        // `singlets` ran beside the others but skipped event 9, which no
        // filter removed.
        let singlets = (0..10).map(|row| (row != 9).then_some(true)).collect();
        let metrics = QcMetrics::of(&mask().with_filter("singlets", singlets), 10, 6);
        let filter = |name: &str| {
            metrics
                .filters
                .iter()
                .find(|filter| filter.name == name)
                .unwrap()
        };
        // Event 0, removed by margins, never reached `PeacoQC`.
        let peacoqc = filter("peacoqc_pass");
        assert_eq!(peacoqc.eligible, 9);
        assert_eq!(peacoqc.eligible_assessed_fraction, Some(1.0));
        // The events `PeacoQC` removed are no longer eligible for margins.
        let margin = filter("margin_pass");
        assert_eq!(margin.eligible, 7);
        assert_eq!(margin.eligible_assessed_fraction, Some(1.0));
        assert_eq!(filter("live").eligible_assessed_fraction, Some(0.0));
        let singlets = filter("singlets");
        assert_eq!(singlets.eligible, 6);
        assert!((singlets.eligible_assessed_fraction.unwrap() - 5.0 / 6.0).abs() < 1e-12);

        let all_removed = EventMask::new(RAW, vec![false; 2], None)
            .with_filter("margin_pass", vec![Some(false); 2])
            .with_filter("live", vec![None; 2]);
        let live = &QcMetrics::of(&all_removed, 2, 0).filters[1];
        assert_eq!((live.eligible, live.eligible_assessed_fraction), (0, None));
    }

    #[test]
    fn checks_counts_and_overall_loss() {
        let checks = evaluate("sample S01", &policy(&[]), &QcMetrics::of(&mask(), 10, 6));
        let status = |name: &str| {
            checks
                .iter()
                .find(|check| check.check == name)
                .map(|check| check.status)
        };
        assert_eq!(status("min_events"), Some(CheckStatus::Pass));
        assert_eq!(status("min_kept_events"), Some(CheckStatus::Fail));
        assert_eq!(status("max_removed_fraction"), Some(CheckStatus::Fail));
    }

    #[test]
    fn checks_filters_with_their_severity() {
        let rule = |required, max: Option<f64>, severity| FilterPolicy {
            required,
            max_removed_fraction: max,
            severity,
            ..FilterPolicy::default()
        };
        let checks = evaluate(
            "sample S01",
            &policy(&[
                ("peacoqc_pass", rule(true, Some(0.25), Severity::Warn)),
                ("margin_pass", rule(true, Some(0.2), Severity::Fail)),
                ("live", rule(true, None, Severity::Fail)),
                ("singlets", rule(false, Some(0.1), Severity::Fail)),
            ]),
            &QcMetrics::of(&mask(), 10, 6),
        );
        let find = |scope: &str, name: &str| {
            checks
                .iter()
                .find(|check| check.scope == scope && check.check == name)
                .unwrap_or_else(|| panic!("{scope} {name}: {checks:?}"))
        };
        let peacoqc = find("filter peacoqc_pass", "filter_removed_fraction");
        assert_eq!(peacoqc.status, CheckStatus::Warn, "3 of 9 > 25%, warn only");
        assert_eq!(peacoqc.limit, Some(0.25));
        assert_eq!(
            find("filter margin_pass", "filter_removed_fraction").status,
            CheckStatus::Pass
        );
        let live = find("filter live", "filter_assessed");
        assert_eq!(live.status, CheckStatus::Fail);
        assert!(
            live.message.contains("assessed no event"),
            "{}",
            live.message
        );
        let singlets = find("filter singlets", "filter_assessed");
        assert_eq!(
            singlets.status,
            CheckStatus::Warn,
            "an absent optional filter has no successful assessment"
        );
    }

    #[test]
    fn checks_the_share_of_raw_events_a_filter_assessed() {
        let rule = |min: f64, severity| FilterPolicy {
            min_assessed_fraction: Some(min),
            severity,
            ..FilterPolicy::default()
        };
        let checks = evaluate(
            "sample S01",
            &policy(&[
                ("peacoqc_pass", rule(0.95, Severity::Warn)),
                ("margin_pass", rule(1.0, Severity::Fail)),
                ("live", rule(0.5, Severity::Fail)),
                ("singlets", rule(0.5, Severity::Fail)),
            ]),
            &QcMetrics::of(&mask(), 10, 6),
        );
        let find = |scope: &str| {
            checks
                .iter()
                .find(|check| check.scope == scope && check.check == "filter_assessed_fraction")
        };
        let peacoqc = find("filter peacoqc_pass").expect("peacoqc_pass");
        assert_eq!(peacoqc.status, CheckStatus::Warn, "9 of 10 < 95%");
        assert_eq!((peacoqc.value, peacoqc.limit), (Some(0.9), Some(0.95)));
        assert_eq!(
            find("filter margin_pass").map(|check| check.status),
            Some(CheckStatus::Pass)
        );
        assert_eq!(
            find("filter live").map(|check| check.status),
            Some(CheckStatus::Fail),
            "an optional filter in the mask that assessed nothing"
        );
        assert!(find("filter singlets").is_none(), "optional and absent");
    }

    #[test]
    fn checks_the_share_of_eligible_events_a_filter_assessed() {
        let rule = |min: f64| FilterPolicy {
            min_assessed_fraction: Some(min),
            assessed_of: AssessedOf::Eligible,
            ..FilterPolicy::default()
        };
        let singlets = (0..10).map(|row| (row != 9).then_some(true)).collect();
        let checks = evaluate(
            "sample S01",
            &policy(&[
                ("peacoqc_pass", rule(1.0)),
                ("singlets", rule(0.9)),
                ("live", rule(0.5)),
            ]),
            &QcMetrics::of(&mask().with_filter("singlets", singlets), 10, 6),
        );
        let find = |scope: &str| {
            checks
                .iter()
                .find(|check| check.scope == scope && check.check == "filter_assessed_fraction")
                .unwrap_or_else(|| panic!("{scope}: {checks:?}"))
        };
        let peacoqc = find("filter peacoqc_pass");
        assert_eq!(
            peacoqc.status,
            CheckStatus::Pass,
            "9 of 10 raw events, but every one margins left"
        );
        assert!(
            peacoqc
                .message
                .contains("100.0% of the events no other filter removed"),
            "{}",
            peacoqc.message
        );
        let singlets = find("filter singlets");
        assert_eq!(singlets.status, CheckStatus::Fail, "5 of 6 < 90%");
        assert_eq!(find("filter live").status, CheckStatus::Fail);
    }
}
