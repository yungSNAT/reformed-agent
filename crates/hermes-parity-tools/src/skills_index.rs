//! Check freshness and source floors of `website/static/api/skills-index.json`.
//! Port of `scripts/check-skills-index-freshness.mjs`.

use std::fs;

use clap::Args;
use jiff::civil::Date;
use jiff::Timestamp;
use serde_json::Value;

use crate::script_helpers::resolve_repo_root;

const FLOORS: [(&str, usize); 6] = [
    ("skills.sh", 100),
    ("lobehub", 100),
    ("clawhub", 50),
    ("official", 50),
    ("github", 30),
    ("browse-sh", 50),
];

#[derive(Args, Debug)]
pub struct CheckSkillsIndexArgs {
    /// Repository root path
    #[arg(long, default_value = ".")]
    repo_root: String,
}

fn env_number(name: &str, default: &str) -> f64 {
    let raw = std::env::var(name).unwrap_or_default();
    let chosen = if raw.is_empty() { default } else { &raw };
    chosen.trim().parse::<f64>().unwrap_or(f64::NAN)
}

fn parse_js_date_ms(raw: &str) -> Option<f64> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(ts) = trimmed.parse::<Timestamp>() {
        return Some(ts.as_millisecond() as f64);
    }
    if let Ok(date) = trimmed.parse::<Date>() {
        if let Ok(ts) = date.at(0, 0, 0, 0).to_zoned(jiff::tz::TimeZone::UTC) {
            return Some(ts.timestamp().as_millisecond() as f64);
        }
    }
    None
}

fn js_string(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) | Some(Value::Bool(false)) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) if n.as_i64() == Some(0) || n.as_f64() == Some(0.0) => String::new(),
        Some(other) => other.to_string(),
    }
}

fn format_js_number(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

pub fn run(args: CheckSkillsIndexArgs) -> Result<i32, String> {
    let repo_root = resolve_repo_root(&args.repo_root)?;
    let index_path = repo_root.join("website/static/api/skills-index.json");
    let raw = match fs::read_to_string(&index_path) {
        Ok(raw) => raw,
        Err(err) => {
            eprintln!("[skills-index] cannot read {}: {err}", index_path.display());
            return Ok(1);
        }
    };
    let data: Value = match serde_json::from_str(&raw) {
        Ok(data) => data,
        Err(err) => {
            eprintln!("[skills-index] cannot read {}: {err}", index_path.display());
            return Ok(1);
        }
    };
    match evaluate(&data) {
        Ok(line) => {
            println!("{line}");
            Ok(0)
        }
        Err(msg) => {
            eprintln!("[skills-index] {msg}");
            Ok(1)
        }
    }
}

pub(crate) fn evaluate(data: &Value) -> Result<String, String> {
    let max_age_hours = env_number("SKILLS_INDEX_MAX_AGE_HOURS", "26");
    let min_total = env_number("SKILLS_INDEX_MIN_TOTAL", "1500");

    let Some(skills) = data.get("skills").and_then(Value::as_array) else {
        return Err("invalid shape: skills must be an array".to_string());
    };

    let generated_at = js_string(data.get("generated_at"));
    let age_hours = parse_js_date_ms(&generated_at)
        .filter(|then| then.is_finite())
        .map(|then| (Timestamp::now().as_millisecond() as f64 - then) / 3_600_000.0);

    let mut counts: Vec<(String, usize)> = Vec::new();
    for skill in skills {
        let source = js_string(skill.get("source"));
        if let Some((_, count)) = counts.iter_mut().find(|(s, _)| s == &source) {
            *count += 1;
        } else {
            counts.push((source, 1));
        }
    }
    let count_for = |name: &str| -> usize {
        counts
            .iter()
            .find(|(s, _)| s == name)
            .map_or(0, |(_, c)| *c)
    };

    let mut issues = Vec::new();
    match age_hours {
        None => issues.push("generated_at is missing or invalid".to_string()),
        Some(age) if age > max_age_hours => issues.push(format!(
            "index is {age:.1}h old (limit {}h)",
            format_js_number(max_age_hours)
        )),
        _ => {}
    }
    if (skills.len() as f64) < min_total {
        issues.push(format!(
            "total skills {} < {}",
            skills.len(),
            format_js_number(min_total)
        ));
    }
    for (source, floor) in FLOORS {
        let count = if source == "skills.sh" {
            count_for("skills.sh") + count_for("skills-sh")
        } else {
            count_for(source)
        };
        if count < floor {
            issues.push(format!("{source}: {count} < {floor}"));
        }
    }

    if !issues.is_empty() {
        return Err(issues.join("; "));
    }

    counts.sort_by_key(|a| std::cmp::Reverse(a.1));
    let summary = counts
        .iter()
        .take(8)
        .map(|(source, count)| format!("{source}={count}"))
        .collect::<Vec<_>>()
        .join(", ");
    Ok(format!(
        "[skills-index] ok: {} skills, generated {}, {}",
        skills.len(),
        generated_at,
        summary
    ))
}
