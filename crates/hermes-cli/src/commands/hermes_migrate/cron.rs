//! Hermes `cron/jobs.json` jobs as Reformed cron jobs, all paused.
//!
//! Reformed evaluates schedules in UTC with the `jiff-cron` crate; Hermes evaluates
//! them in its configured zone with croniter. A job converts only when the
//! result fires at the same instants (interval and one-off jobs) or the same
//! wall-clock fields read in UTC (cron jobs, with a warning); anything else is
//! skipped with its reason. Job names and prompts are never printed.

use std::path::{Component, Path};

use hermes_core::time::{parse_rfc3339, utc_civil};
use hermes_cron::{CronJob, DeliverConfig, JobStatus, ModelConfig};
use jiff::{RoundMode, Timestamp, TimestampRound, Unit};
use serde_json::{Map, Value};

use super::super::blueprint_deliver_config;
use super::super::hermes_history::safe_session_id;

/// Job fields Reformed cannot carry and that change what a job does or when.
const UNSUPPORTED: [&str; 11] = [
    "enabled_toolsets",
    "monitor_script",
    "monitor_url",
    "base_url",
    "interpreter",
    "timezone",
    "failure_deliver",
    "attach_to_session",
    "reasoning_effort",
    "grace_minutes",
    "catch_up",
];

/// Every entry of `jobs.json`: `{"jobs": [...]}`, `{"jobs": {id: job}}` or a
/// bare list, as Hermes reads it.
pub(super) fn jobs(bytes: &[u8]) -> Result<Vec<Value>, &'static str> {
    match serde_json::from_slice::<Value>(bytes).map_err(|_| "not valid JSON")? {
        Value::Array(jobs) => Ok(jobs),
        Value::Object(mut data) => match data.remove("jobs") {
            None | Some(Value::Null) => Ok(Vec::new()),
            Some(Value::Array(jobs)) => Ok(jobs),
            Some(Value::Object(by_id)) => Ok(by_id
                .into_iter()
                .map(|(key, mut job)| {
                    if let Value::Object(fields) = &mut job {
                        let inline = fields
                            .get("id")
                            .and_then(Value::as_str)
                            .is_some_and(|id| !id.is_empty());
                        if !inline {
                            fields.insert("id".into(), Value::String(key));
                        }
                    }
                    job
                })
                .collect()),
            Some(_) => Err("`jobs` is neither a list nor a map"),
        },
        _ => Err("neither a job list nor an object"),
    }
}

/// How a job is shown: its id when that is a safe file name, else its position.
pub(super) fn label(job: &Value, position: usize) -> String {
    job.get("id")
        .and_then(Value::as_str)
        .filter(|id| safe_session_id(id))
        .map_or_else(|| format!("#{position}"), str::to_string)
}

/// The paused Reformed job and a note on its schedule, or why it is skipped.
pub(super) fn convert(
    job: &Value,
    now: Timestamp,
    scripts: &Path,
) -> Result<(CronJob, String), String> {
    let fields = job.as_object().ok_or("not a job object")?;
    let id = fields
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| safe_session_id(id))
        .ok_or("its id is not a safe file name")?;
    if let Some(field) = UNSUPPORTED
        .iter()
        .find(|field| fields.get(**field).is_some_and(|value| !value.is_null()))
    {
        return Err(format!("sets {field}, which Reformed cannot carry"));
    }
    if fields
        .get("repeat")
        .is_some_and(|repeat| !repeat.is_null() && !repeat.is_object())
    {
        return Err("repeat is not an object".into());
    }
    let times = match fields.get("repeat").and_then(|repeat| repeat.get("times")) {
        None | Some(Value::Null) => None,
        Some(times) => Some(count(times, "repeat.times")?),
    };
    let completed = match fields
        .get("repeat")
        .and_then(|repeat| repeat.get("completed"))
    {
        None | Some(Value::Null) => 0,
        Some(completed) => count(completed, "repeat.completed")?,
    };
    if fields.get("state").and_then(Value::as_str) == Some("completed")
        || times.is_some_and(|times| completed >= times)
    {
        return Err("it has finished".into());
    }
    let schedule = fields
        .get("schedule")
        .and_then(Value::as_object)
        .ok_or("it has no schedule")?;
    let (expression, note, once) = match schedule.get("kind").and_then(Value::as_str) {
        Some("cron") => {
            let expression = cron_expression(schedule.get("expr"))?;
            let note = format!(
                "{expression}, now read in UTC; Hermes read it in its configured time zone"
            );
            (expression, note, false)
        }
        Some("interval") => {
            let minutes = schedule
                .get("minutes")
                .and_then(Value::as_u64)
                .filter(|minutes| *minutes > 0)
                .ok_or("its interval has no minutes")?;
            let expression = interval(minutes, anchor(fields))?;
            let note = format!("every {minutes} minutes, as {expression} in UTC");
            (expression, note, false)
        }
        Some("once") => {
            if completed > 0 {
                return Err("it has finished".into());
            }
            let at = schedule
                .get("run_at")
                .and_then(Value::as_str)
                .and_then(parse_rfc3339)
                .ok_or("its one-off time is not a time with a zone")?;
            let at = whole_second(at)
                .ok_or("its one-off time is after the latest time Reformed can schedule")?;
            if at <= now {
                return Err("its one-off time has passed".into());
            }
            let civil = utc_civil(at);
            let expression = format!(
                "{} {} {} {} {} * {}",
                civil.second(),
                civil.minute(),
                civil.hour(),
                civil.day(),
                civil.month(),
                civil.year()
            );
            let note = format!("once, at {} UTC", at.strftime("%Y-%m-%d %H:%M:%S"));
            (expression, note, true)
        }
        _ => return Err("its schedule kind is unknown".into()),
    };

    let mut converted = CronJob::new(expression, text(fields, "prompt")?.unwrap_or_default());
    converted.id = id.to_string();
    converted.name = text(fields, "name")?;
    converted.skills = skills(fields)?;
    let (provider, model) = (text(fields, "provider")?, text(fields, "model")?);
    converted.model =
        (provider.is_some() || model.is_some()).then_some(ModelConfig { provider, model });
    converted.deliver = deliver(fields)?;
    converted.status = JobStatus::Paused;
    if let Some(created) = fields
        .get("created_at")
        .and_then(Value::as_str)
        .and_then(parse_rfc3339)
    {
        converted.created_at = created;
    }
    converted.repeat = if once { Some(1) } else { times };
    converted.run_count = completed;
    converted.script = script(fields, scripts)?;
    converted.no_agent = match fields.get("no_agent") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(no_agent)) => *no_agent,
        Some(_) => return Err("no_agent is not true or false".into()),
    };
    converted.workdir = text(fields, "workdir")?;
    converted.context_from = match fields.get("context_from") {
        None | Some(Value::Null) => None,
        Some(Value::String(id)) if id.trim().is_empty() => None,
        Some(Value::String(id)) => Some(vec![id.clone()]),
        Some(Value::Array(ids)) => Some(strings(ids, "context_from")?),
        Some(_) => return Err("context_from is not a list of job ids".into()),
    };
    converted
        .normalize_paths()
        .map_err(|_| "Reformed rejects its workdir")?;
    converted
        .validate()
        .map_err(|_| "Reformed rejects its schedule or fields")?;
    Ok((converted, note))
}

fn count(value: &Value, field: &str) -> Result<u32, String> {
    value
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| format!("{field} is not a whole number"))
}

/// A text field, with an absent, null or blank value read as unset.
fn text(fields: &Map<String, Value>, field: &str) -> Result<Option<String>, String> {
    match fields.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if value.trim().is_empty() => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(format!("{field} is not text")),
    }
}

fn strings(values: &[Value], field: &str) -> Result<Vec<String>, String> {
    values
        .iter()
        .map(|value| value.as_str().map(str::to_string))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| format!("{field} is not a list of text"))
}

fn skills(fields: &Map<String, Value>) -> Result<Option<Vec<String>>, String> {
    let skills = match fields.get("skills") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(skills)) => strings(skills, "skills")?,
        Some(_) => return Err("skills is not a list of text".into()),
    };
    if !skills.is_empty() {
        return Ok(Some(skills));
    }
    Ok(text(fields, "skill")?.map(|skill| vec![skill]))
}

/// Hermes's 5-field expression, or its 6-field form (croniter puts seconds
/// last) when the seconds field is 0.
fn cron_expression(expression: Option<&Value>) -> Result<String, String> {
    let expression = expression
        .and_then(Value::as_str)
        .ok_or("its schedule has no cron expression")?;
    let fields: Vec<&str> = expression.split_whitespace().collect();
    let fields = match fields.as_slice() {
        [minute, hour, day, month, weekday] | [minute, hour, day, month, weekday, "0"] => {
            [*minute, *hour, *day, *month, *weekday]
        }
        [_, _, _, _, _, _] => return Err("it runs at seconds past the minute".into()),
        _ => return Err("its cron expression does not have 5 fields".into()),
    };
    Ok(fields.join(" "))
}

/// The time an interval job's runs are counted from: its next run, else its
/// last run, else its creation.
fn anchor(fields: &Map<String, Value>) -> Option<Timestamp> {
    ["next_run_at", "last_run_at", "created_at"]
        .iter()
        .filter_map(|field| fields.get(*field).and_then(Value::as_str))
        .find_map(parse_rfc3339)
        .and_then(whole_second)
}

/// `at` rounded up to a whole second, as Reformed's schedules have no
/// fractions: a converted job then never runs before its Hermes time.
/// `None` when that second is past the latest supported instant.
fn whole_second(at: Timestamp) -> Option<Timestamp> {
    at.round(
        TimestampRound::new()
            .smallest(Unit::Second)
            .mode(RoundMode::Ceil),
    )
    .ok()
}

/// An interval that divides the hour or the day, as a UTC expression with
/// seconds firing at the same whole seconds as from `at`, which
/// `whole_second` has rounded up.
fn interval(minutes: u64, at: Option<Timestamp>) -> Result<String, String> {
    let hourly = minutes < 60 && 60_u64.is_multiple_of(minutes);
    if !hourly && !(minutes.is_multiple_of(60) && 1440_u64.is_multiple_of(minutes)) {
        return Err(format!(
            "it runs every {minutes} minutes, which does not divide an hour or a day"
        ));
    }
    let at = at.ok_or("its interval has no next, last or creation time to keep its timing")?;
    let list = |start: u64, step: u64, span: u64| {
        (0..span / step)
            .map(|k| (start % step + k * step).to_string())
            .collect::<Vec<_>>()
            .join(",")
    };
    let at = utc_civil(at);
    let (second, minute, hour) = (
        at.second(),
        u64::from(at.minute().unsigned_abs()),
        u64::from(at.hour().unsigned_abs()),
    );
    Ok(match minutes {
        1 => format!("{second} * * * * *"),
        60 => format!("{second} {minute} * * * *"),
        _ if hourly => format!("{second} {} * * * *", list(minute, minutes, 60)),
        _ => format!("{second} {minute} {} * * *", list(hour, minutes / 60, 24)),
    })
}

fn deliver(fields: &Map<String, Value>) -> Result<Option<DeliverConfig>, String> {
    let raw = match fields.get("deliver") {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::String(raw)) if raw.trim().is_empty() => return Ok(None),
        Some(Value::String(raw)) => raw.trim(),
        Some(_) => return Err("deliver is not text".into()),
    };
    if raw.contains(',') {
        return Err("it delivers to several targets".into());
    }
    let target = if raw.eq_ignore_ascii_case("origin") {
        let origin = fields
            .get("origin")
            .and_then(Value::as_object)
            .ok_or("it delivers to its origin, which Hermes did not record")?;
        if origin
            .get("thread_id")
            .is_some_and(|thread| !thread.is_null())
        {
            return Err("it delivers to a thread".into());
        }
        let platform = origin.get("platform").and_then(Value::as_str);
        let chat = match origin.get("chat_id") {
            Some(Value::String(chat)) => Some(chat.clone()),
            Some(Value::Number(chat)) => Some(chat.to_string()),
            _ => None,
        };
        let (Some(platform), Some(chat)) = (platform, chat) else {
            return Err("it delivers to its origin, which Hermes did not record".into());
        };
        format!("{platform}:{chat}")
    } else {
        raw.to_string()
    };
    blueprint_deliver_config(&target)
        .map(Some)
        .ok_or_else(|| "it delivers to a platform Reformed does not support".into())
}

/// True when `root` and every folder on the way to `relative` are real
/// folders and `relative` is a regular file: no link is followed, so the
/// file is one the `scripts` tree copy also copies.
fn regular_file_under(root: &Path, relative: &Path) -> bool {
    let mut current = root.to_path_buf();
    if !std::fs::symlink_metadata(&current).is_ok_and(|meta| meta.is_dir()) {
        return false;
    }
    let mut parts = relative.components().peekable();
    while let Some(part) = parts.next() {
        current.push(part);
        let Ok(meta) = std::fs::symlink_metadata(&current) else {
            return false;
        };
        let expected = if parts.peek().is_some() {
            meta.is_dir()
        } else {
            meta.is_file()
        };
        if !expected {
            return false;
        }
    }
    true
}

/// A script file in Hermes's `scripts` folder, written as a path so Reformed
/// never reads the name as inline shell.
fn script(fields: &Map<String, Value>, scripts: &Path) -> Result<Option<String>, String> {
    let Some(name) = text(fields, "script")? else {
        return Ok(None);
    };
    let path = Path::new(&name);
    let inside = !name.starts_with('~')
        && !name.chars().any(char::is_whitespace)
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
    if !inside || !regular_file_under(scripts, path) {
        return Err("its script is not a file in the scripts folder".into());
    }
    Ok(Some(if name.contains(['/', '\\']) {
        name
    } else {
        format!("./{name}")
    }))
}
