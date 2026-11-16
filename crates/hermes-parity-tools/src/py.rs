//! The parts of Python's behaviour the ported scripts' output depends on:
//! `json.dumps` byte for byte (`indent=2` or compact, `ensure_ascii`,
//! `sort_keys`), `str()` of a parsed JSON value, `str.strip()`,
//! `datetime.isoformat()` and `pathlib.Path.resolve()`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// A JSON value that keeps its keys in insertion order, as a Python dict does.
#[derive(Debug, Clone, PartialEq)]
pub enum Py {
    Null,
    Bool(bool),
    Int(i128),
    Float(f64),
    Str(String),
    List(Vec<Py>),
    Dict(Vec<(String, Py)>),
}

impl Py {
    pub fn dict<K: Into<String>>(pairs: impl IntoIterator<Item = (K, Py)>) -> Py {
        Py::Dict(pairs.into_iter().map(|(k, v)| (k.into(), v)).collect())
    }

    pub fn strs<S: AsRef<str>>(items: impl IntoIterator<Item = S>) -> Py {
        Py::List(items.into_iter().map(|s| Py::from(s.as_ref())).collect())
    }

    /// Sets `key` as a Python dict assignment does: in place when present,
    /// otherwise appended.
    pub fn set(&mut self, key: &str, value: Py) {
        if let Py::Dict(pairs) = self {
            match pairs.iter_mut().find(|(k, _)| k == key) {
                Some(slot) => slot.1 = value,
                None => pairs.push((key.to_string(), value)),
            }
        }
    }

    pub fn get(&self, key: &str) -> Option<&Py> {
        match self {
            Py::Dict(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
}

impl From<&str> for Py {
    fn from(value: &str) -> Self {
        Py::Str(value.to_string())
    }
}

impl From<String> for Py {
    fn from(value: String) -> Self {
        Py::Str(value)
    }
}

impl From<bool> for Py {
    fn from(value: bool) -> Self {
        Py::Bool(value)
    }
}

impl From<usize> for Py {
    fn from(value: usize) -> Self {
        Py::Int(value as i128)
    }
}

impl From<f64> for Py {
    fn from(value: f64) -> Self {
        Py::Float(value)
    }
}

/// A parsed JSON value as Python's `json.loads` gives it. Object keys come
/// back sorted, as serde_json keeps them; only `sort_keys` output reads them.
pub fn from_json(value: &Value) -> Py {
    match value {
        Value::Null => Py::Null,
        Value::Bool(b) => Py::Bool(*b),
        Value::Number(n) => match (n.as_i64(), n.as_u64()) {
            (Some(i), _) => Py::Int(i as i128),
            (None, Some(u)) => Py::Int(u as i128),
            _ => Py::Float(n.as_f64().unwrap_or(f64::NAN)),
        },
        Value::String(s) => Py::Str(s.clone()),
        Value::Array(items) => Py::List(items.iter().map(from_json).collect()),
        Value::Object(map) => {
            Py::Dict(map.iter().map(|(k, v)| (k.clone(), from_json(v))).collect())
        }
    }
}

/// `json.dumps(value, indent=indent, sort_keys=sort_keys)` with Python's
/// defaults otherwise: `ensure_ascii=True`, and `", "` between items when
/// there is no indent.
pub fn dumps(value: &Py, indent: Option<usize>, sort_keys: bool) -> String {
    let mut out = String::new();
    write_value(&mut out, value, indent, sort_keys, 0);
    out
}

fn newline(out: &mut String, indent: Option<usize>, level: usize) {
    if let Some(width) = indent {
        out.push('\n');
        out.push_str(&" ".repeat(width * level));
    }
}

fn write_value(out: &mut String, value: &Py, indent: Option<usize>, sort: bool, level: usize) {
    let item_sep = if indent.is_some() { "," } else { ", " };
    match value {
        Py::Null => out.push_str("null"),
        Py::Bool(true) => out.push_str("true"),
        Py::Bool(false) => out.push_str("false"),
        Py::Int(i) => {
            let _ = write!(out, "{i}");
        }
        Py::Float(f) => out.push_str(&float_json(*f)),
        Py::Str(s) => write_string(out, s),
        Py::List(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(item_sep);
                }
                newline(out, indent, level + 1);
                write_value(out, item, indent, sort, level + 1);
            }
            newline(out, indent, level);
            out.push(']');
        }
        Py::Dict(pairs) => {
            if pairs.is_empty() {
                out.push_str("{}");
                return;
            }
            let mut ordered: Vec<&(String, Py)> = pairs.iter().collect();
            if sort {
                ordered.sort_by(|a, b| a.0.cmp(&b.0));
            }
            out.push('{');
            for (i, (key, item)) in ordered.into_iter().enumerate() {
                if i > 0 {
                    out.push_str(item_sep);
                }
                newline(out, indent, level + 1);
                write_string(out, key);
                out.push_str(": ");
                write_value(out, item, indent, sort, level + 1);
            }
            newline(out, indent, level);
            out.push('}');
        }
    }
}

fn write_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            ' '..='~' => out.push(c),
            _ => {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
        }
    }
    out.push('"');
}

fn float_json(f: f64) -> String {
    if f.is_nan() {
        "NaN".to_string()
    } else if f.is_infinite() {
        if f > 0.0 { "Infinity" } else { "-Infinity" }.to_string()
    } else {
        float_repr(f)
    }
}

/// Python's `repr(float)`: the shortest digits that round-trip, written in
/// fixed notation when the decimal exponent is from -4 to 15, otherwise as
/// `1.5e+16` or `1e-05`.
pub fn float_repr(f: f64) -> String {
    if f == 0.0 {
        return if f.is_sign_negative() { "-0.0" } else { "0.0" }.to_string();
    }
    let sci = format!("{f:e}");
    let (mantissa, exp) = sci.split_once('e').expect("{:e} always has an exponent");
    let exp: i32 = exp.parse().expect("{:e} exponent is an integer");
    let negative = mantissa.starts_with('-');
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let sign = if negative { "-" } else { "" };
    if (-4..16).contains(&exp) {
        let body = if exp >= 0 {
            let int_len = exp as usize + 1;
            if digits.len() > int_len {
                format!("{}.{}", &digits[..int_len], &digits[int_len..])
            } else {
                format!("{}{}.0", digits, "0".repeat(int_len - digits.len()))
            }
        } else {
            format!("0.{}{}", "0".repeat((-exp - 1) as usize), digits)
        };
        format!("{sign}{body}")
    } else {
        let mantissa = mantissa.trim_start_matches('-');
        let exp_sign = if exp < 0 { '-' } else { '+' };
        format!("{sign}{mantissa}e{exp_sign}{:02}", exp.abs())
    }
}

/// Python's `str.strip()` with no argument: Unicode whitespace and the
/// separators U+001C to U+001F.
pub fn strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
}

/// Python's `str()` of a value `json.loads` produced.
pub fn str_of(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => repr(other),
    }
}

/// Python's `repr()` of a value `json.loads` produced.
pub fn repr(value: &Value) -> String {
    match value {
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Number(n) => match from_json(value) {
            Py::Float(f) => float_repr(f),
            _ => n.to_string(),
        },
        Value::String(s) => str_repr(s),
        Value::Array(items) => {
            let inner: Vec<String> = items.iter().map(repr).collect();
            format!("[{}]", inner.join(", "))
        }
        Value::Object(map) => {
            let inner: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{}: {}", str_repr(k), repr(v)))
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
    }
}

fn str_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::new();
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                let _ = write!(out, "\\x{:02x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// `datetime.now(timezone.utc).isoformat()`.
pub fn utc_isoformat() -> String {
    isoformat(chrono::Utc::now())
}

pub fn isoformat(now: chrono::DateTime<chrono::Utc>) -> String {
    if now.timestamp_subsec_micros() == 0 {
        now.format("%Y-%m-%dT%H:%M:%S+00:00").to_string()
    } else {
        now.format("%Y-%m-%dT%H:%M:%S%.6f+00:00").to_string()
    }
}

/// `datetime.now(timezone.utc).strftime("%Y%m%d-%H%M%S")`.
pub fn utc_stamp() -> String {
    chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string()
}

/// `pathlib.Path(raw).expanduser()`.
pub fn expanduser(raw: &str) -> PathBuf {
    let home = || {
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
    };
    if raw == "~" {
        if let Some(home) = home() {
            return home;
        }
    } else if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
        if let Some(home) = home() {
            return home.join(rest);
        }
    }
    PathBuf::from(raw)
}

/// `pathlib.Path.resolve()`: absolute, with symbolic links resolved as far as
/// the path exists; a missing tail is appended as written.
pub fn resolve(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut existing = absolute.as_path();
    let mut tail = Vec::new();
    loop {
        if let Ok(real) = existing.canonicalize() {
            let mut out = strip_verbatim(real);
            for part in tail.iter().rev() {
                out.push(part);
            }
            return out;
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                tail.push(name.to_os_string());
                existing = parent;
            }
            _ => return absolute,
        }
    }
}

/// Drops the `\\?\` prefix Windows' canonical paths carry, which Python's
/// `resolve()` does not show.
fn strip_verbatim(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
        _ => path,
    }
}

/// `base / rel` as pathlib joins: each part of `rel` is appended with the
/// platform's separator, and an absolute `rel` replaces `base`.
pub fn join(base: &Path, rel: &str) -> PathBuf {
    let mut out = base.to_path_buf();
    for part in Path::new(rel).components() {
        out.push(part);
    }
    out
}

/// `str(path)` as Python prints it.
pub fn path_str(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests;
