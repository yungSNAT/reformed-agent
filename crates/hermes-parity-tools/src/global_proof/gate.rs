use std::collections::BTreeSet;

use crate::py::{self, Py};

use super::{entries, items, number, truth, Metrics};

struct Check {
    metric: String,
    status: &'static str,
    actual: Py,
    limit: Py,
    details: Vec<(&'static str, Py)>,
}

impl Check {
    fn into_report(self) -> Py {
        Py::dict(
            [
                ("metric", self.metric.into()),
                ("status", self.status.into()),
                ("actual", self.actual),
                ("limit", self.limit),
            ]
            .into_iter()
            .chain(self.details),
        )
    }

    fn warn(&mut self, rule: &'static str) {
        self.status = "warn";
        self.details.push(("special_rule", rule.into()));
    }
}

pub(super) struct Gate {
    pub pass: bool,
    pub mode: String,
    checks: Vec<Check>,
    applied_rule: Option<Py>,
}

impl Gate {
    pub fn evaluate(metrics: &Metrics, thresholds: &Py) -> Result<Self, String> {
        let mut checks = Vec::new();
        let nested = thresholds
            .get("metric_thresholds")
            .filter(|value| value.is_dict());
        for (metric, limit) in entries(nested.unwrap_or(thresholds)) {
            if nested.is_none()
                && matches!(
                    metric.as_str(),
                    "required_workstreams_complete" | "gate_mode" | "special_rules"
                )
            {
                continue;
            }
            let actual = metrics
                .iter()
                .find(|(key, _)| key == metric)
                .map(|(_, value)| *value);
            let status = if let Some(actual) = actual {
                let limit = number(limit)?;
                let ok = if metric.starts_with("min_") {
                    actual >= limit
                } else {
                    actual <= limit
                };
                if ok {
                    "pass"
                } else {
                    "fail"
                }
            } else {
                "missing"
            };
            checks.push(Check {
                metric: metric.clone(),
                status,
                actual: actual.map_or(Py::Null, Py::from),
                limit: limit.clone(),
                details: Vec::new(),
            });
        }
        let mode = thresholds
            .get("gate_mode")
            .map_or_else(String::new, py::str_of);
        let mode = py::strip(&mode).to_lowercase();
        Ok(Self {
            pass: checks.iter().all(|check| check.status == "pass"),
            mode: if mode.is_empty() {
                "legacy".into()
            } else {
                mode
            },
            checks,
            applied_rule: None,
        })
    }

    pub fn require_workstreams(&mut self, thresholds: &Py, completion: &Py) -> Result<(), String> {
        let mut actual = Py::dict::<&str>([]);
        let mut ok = true;
        if let Some(required) = thresholds.get("required_workstreams_complete") {
            for item in items(required, "required_workstreams_complete")? {
                let Py::Str(key) = item else {
                    return Err("required workstreams must be strings".into());
                };
                let completed = completion.get(&key).is_some_and(truth);
                actual.set(&key, completed.into());
                ok &= completed;
            }
        }
        self.pass &= ok;
        self.checks.push(Check {
            metric: "required_workstreams_complete".into(),
            status: if ok { "pass" } else { "fail" },
            actual,
            limit: "all true".into(),
            details: Vec::new(),
        });
        Ok(())
    }

    pub fn into_report(self) -> Py {
        let mut report = Py::dict([
            (
                "checks",
                Py::List(self.checks.into_iter().map(Check::into_report).collect()),
            ),
            ("pass", self.pass.into()),
            ("mode", self.mode.into()),
        ]);
        if let Some(rule) = self.applied_rule {
            report.set("special_rules_applied", Py::List(vec![rule]));
        }
        report
    }
}

const TREE_DRIFT: &str = "allow_tree_drift_when_functional_clean";
const FILE_OVERSHOOT: &str = "allow_files_only_upstream_overshoot_when_functional_clean";
const TREE_LIMITS: &[(&str, f64)] = &[
    ("max_queue_pending_commits", 0.0),
    ("max_unowned_divergences", 0.0),
    ("max_divergence_review_overdue", 0.0),
    ("min_test_coverage_tracked_behavior_ratio", 1.0),
    ("min_sota_harness_domain_coverage_ratio", 1.0),
    ("min_behavioral_similarity_ratio", 1.0),
    ("max_behavioral_regressions", 0.0),
    ("max_behavioral_unverified_cases", 0.0),
    ("min_deep_problem_solving_ratio", 1.0),
    ("max_deep_problem_solving_gaps", 0.0),
    ("max_deep_problem_solving_unverified_cases", 0.0),
];
const FILE_LIMITS: &[(&str, f64)] = &[
    ("max_unowned_divergences", 0.0),
    ("max_divergence_review_overdue", 0.0),
    ("max_queue_pending_commits", 100.0),
    ("min_test_intent_mapping_ratio", 0.9),
];

fn metric_limits(config: &Py, limits: &[(&str, f64)]) -> Result<Vec<f64>, String> {
    limits
        .iter()
        .map(|(key, default)| {
            config
                .get(&format!("requires_{key}"))
                .map_or(Ok(*default), number)
        })
        .collect()
}

fn matches_limits(metrics: &Metrics, limits: &[(&str, f64)], configured: &[f64]) -> bool {
    limits.iter().zip(configured).all(|((key, _), limit)| {
        let actual = metrics
            .iter()
            .find(|(name, _)| name == key)
            .map_or(0.0, |(_, value)| *value);
        if key.starts_with("min_") {
            actual >= *limit
        } else {
            actual <= *limit
        }
    })
}

fn enabled_rule<'a>(rules: &'a Py, key: &str) -> Option<&'a Py> {
    rules
        .get(key)
        .filter(|rule| rule.get("enabled").is_some_and(truth))
}

fn permits_without_release(config: &Py) -> bool {
    config
        .get("requires_release_gate_pass")
        .is_some_and(|value| !truth(value))
}

pub(super) fn apply_ci_rules(
    metrics: &Metrics,
    ci: &mut Gate,
    release: &Gate,
    thresholds: &Py,
) -> Result<(), String> {
    let Some(rules) = thresholds.get("special_rules") else {
        return Ok(());
    };
    let failed: BTreeSet<_> = ci
        .checks
        .iter()
        .filter(|check| check.status == "fail")
        .map(|check| py::strip(&check.metric).to_owned())
        .collect();
    if let Some(config) = enabled_rule(rules, TREE_DRIFT) {
        let allowed = if let Some(raw) = config.get("metrics") {
            items(raw, "tree drift metrics")?
                .iter()
                .map(py::str_of)
                .collect()
        } else {
            vec![
                "max_commits_behind".into(),
                "max_upstream_patch_missing".into(),
            ]
        };
        let allowed: BTreeSet<_> = allowed
            .into_iter()
            .map(|name| py::strip(&name).to_owned())
            .filter(|name| !name.is_empty())
            .collect();
        let limits = metric_limits(config, TREE_LIMITS)?;
        if !failed.is_empty()
            && failed.is_subset(&allowed)
            && (release.pass || permits_without_release(config))
            && matches_limits(metrics, TREE_LIMITS, &limits)
        {
            for check in ci.checks.iter_mut().filter(|check| check.status == "fail") {
                check.warn(TREE_DRIFT);
            }
            ci.pass = true;
            ci.applied_rule = Some(Py::dict([
                ("rule", TREE_DRIFT.into()),
                ("metrics", Py::strs(failed)),
                ("reason", "functional parity clean; raw upstream tree drift kept as observability warning for the Rust fork".into()),
            ]));
            return Ok(());
        }
    }

    let Some(config) = enabled_rule(rules, FILE_OVERSHOOT) else {
        return Ok(());
    };
    if failed.len() != 1 || !failed.contains("max_files_only_upstream") {
        return Ok(());
    }
    let Some(check) = ci
        .checks
        .iter_mut()
        .rev()
        .find(|check| py::strip(&check.metric) == "max_files_only_upstream")
    else {
        return Ok(());
    };
    let (Ok(actual), Ok(limit)) = (number(&check.actual), number(&check.limit)) else {
        return Ok(());
    };
    let overshoot = (actual - limit).max(0.0);
    let max_overshoot = config.get("max_overshoot").map_or(Ok(0.0), number)?;
    if overshoot > max_overshoot || (!release.pass && !permits_without_release(config)) {
        return Ok(());
    }
    let limits = metric_limits(config, FILE_LIMITS)?;
    if !matches_limits(metrics, FILE_LIMITS, &limits) {
        return Ok(());
    }
    check.warn(FILE_OVERSHOOT);
    check.details.extend([
        ("overshoot", overshoot.into()),
        ("max_overshoot", max_overshoot.into()),
    ]);
    ci.pass = true;
    ci.applied_rule = Some(Py::dict([
        ("rule", FILE_OVERSHOOT.into()),
        ("metric", "max_files_only_upstream".into()),
        ("actual", actual.into()),
        ("limit", limit.into()),
        ("overshoot", overshoot.into()),
        ("max_overshoot", max_overshoot.into()),
        (
            "reason",
            "functional parity clean; upstream-only file growth treated as drift warning".into(),
        ),
    ]));
    Ok(())
}
