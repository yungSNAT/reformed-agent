//! The parts of Python's behaviour the ported scripts' output depends on:
//! `json.loads` and `json.dumps` byte for byte (`indent=2` or compact,
//! `ensure_ascii`, `sort_keys`), `str()` of a parsed JSON value,
//! `str.strip()`, `datetime.isoformat()` and `pathlib.Path.resolve()`.
//!
//! `repr` of a string escapes each character `str.isprintable()` rejects,
//! by the Unicode 17 categories Python 3.15 uses; a Python on older Unicode
//! data also escapes the characters assigned since.

use std::fmt::Write as _;

use finl_unicode::categories::CharacterCategories as _;
use std::path::{Path, PathBuf};

mod loads;
pub use loads::loads;

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

    pub fn is_dict(&self) -> bool {
        matches!(self, Py::Dict(_))
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
    write_utf16_string(out, s.encode_utf16());
}

/// Encode a Python JSON string with `ensure_ascii=True`, including any lone
/// UTF-16 surrogates such as the ones Python uses for undecodable path bytes.
pub fn json_utf16_string(units: impl IntoIterator<Item = u16>) -> String {
    let mut out = String::new();
    write_utf16_string(&mut out, units);
    out
}

fn write_utf16_string(out: &mut String, units: impl IntoIterator<Item = u16>) {
    out.push('"');
    for unit in units {
        match unit {
            0x22 => out.push_str("\\\""),
            0x5c => out.push_str("\\\\"),
            0x0a => out.push_str("\\n"),
            0x0d => out.push_str("\\r"),
            0x09 => out.push_str("\\t"),
            0x08 => out.push_str("\\b"),
            0x0c => out.push_str("\\f"),
            0x20..=0x7e => out.push(char::from(unit as u8)),
            _ => {
                let _ = write!(out, "\\u{unit:04x}");
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
    if f.is_nan() {
        return "nan".to_string();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf" } else { "-inf" }.to_string();
    }
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
pub fn str_of(value: &Py) -> String {
    match value {
        Py::Str(s) => s.clone(),
        other => repr(other),
    }
}

/// Python's `repr()` of a value `json.loads` produced.
pub fn repr(value: &Py) -> String {
    match value {
        Py::Null => "None".to_string(),
        Py::Bool(true) => "True".to_string(),
        Py::Bool(false) => "False".to_string(),
        Py::Int(i) => i.to_string(),
        Py::Float(f) => float_repr(*f),
        Py::Str(s) => str_repr(s),
        Py::List(items) => {
            let inner: Vec<String> = items.iter().map(repr).collect();
            format!("[{}]", inner.join(", "))
        }
        Py::Dict(pairs) => {
            let inner: Vec<String> = pairs
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
            c if !is_printable(c) => {
                let _ = match c as u32 {
                    n @ ..=0xff => write!(out, "\\x{n:02x}"),
                    n @ ..=0xffff => write!(out, "\\u{n:04x}"),
                    n => write!(out, "\\U{n:08x}"),
                };
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Python's `str.isprintable()` for one character: letters, marks, numbers,
/// punctuation, symbols and the space.
fn is_printable(c: char) -> bool {
    c == ' ' || c.is_letter() || c.is_mark() || c.is_number() || c.is_punctuation() || c.is_symbol()
}

/// `datetime.now(timezone.utc).isoformat()`.
pub fn utc_isoformat() -> String {
    isoformat(jiff::Timestamp::now())
}

pub fn isoformat(now: jiff::Timestamp) -> String {
    if now.subsec_microsecond() == 0 {
        now.strftime("%Y-%m-%dT%H:%M:%S+00:00").to_string()
    } else {
        now.strftime("%Y-%m-%dT%H:%M:%S%.6f+00:00").to_string()
    }
}

/// `datetime.now(timezone.utc).strftime("%Y%m%d-%H%M%S")`.
pub fn utc_stamp() -> String {
    jiff::Timestamp::now().strftime("%Y%m%d-%H%M%S").to_string()
}

/// `pathlib.Path(raw).expanduser()`: `~` and `~user` become a home
/// directory as `os.path.expanduser` finds it; a home it cannot find is an
/// error, as pathlib's RuntimeError is.
pub fn expanduser(raw: &str) -> Result<PathBuf, String> {
    if !raw.starts_with('~') {
        return Ok(PathBuf::from(raw));
    }
    let seps: &[char] = if cfg!(windows) { &['/', '\\'] } else { &['/'] };
    let end = raw[1..].find(seps).map_or(raw.len(), |i| i + 1);
    let (head, rest) = raw.split_at(end);
    let user = &head[1..];
    let home = if cfg!(windows) {
        windows_home(user)
    } else {
        posix_home(user)
    };
    let home = home.ok_or_else(|| "Could not determine home directory.".to_string())?;
    let home = if cfg!(windows) {
        home
    } else {
        home.trim_end_matches('/').to_string()
    };
    let joined = home + rest;
    Ok(PathBuf::from(if joined.is_empty() { "/" } else { &joined }))
}

/// `posixpath.expanduser`: `$HOME` for `~` (else the current user's passwd
/// entry, as `getpwuid(getuid())`), the named user's passwd entry otherwise.
fn posix_home(user: &str) -> Option<String> {
    if user.is_empty() {
        if let Some(home) = std::env::var_os("HOME") {
            return Some(home.to_string_lossy().into_owned());
        }
    }
    let uid = if user.is_empty() {
        Some(current_uid()?.to_string())
    } else {
        None
    };
    // shortcut: reads /etc/passwd where Python asks NSS (getpwnam,
    // getpwuid); users known only to LDAP or similar are not found.
    let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
    passwd.lines().find_map(|line| {
        let fields: Vec<&str> = line.split(':').collect();
        let found = match &uid {
            Some(uid) => fields.get(2) == Some(&uid.as_str()),
            None => fields[0] == user,
        };
        (fields.len() >= 7 && found).then(|| fields[5].to_string())
    })
}

/// The process's user id, which owns `/proc/self`.
#[cfg(unix)]
fn current_uid() -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    // shortcut: /proc/self gives the effective id where Python asks the
    // real one (they differ only under setuid), and is missing on macOS,
    // where a bare `~` without $HOME is then an error.
    std::fs::metadata("/proc/self").ok().map(|m| m.uid())
}

#[cfg(not(unix))]
fn current_uid() -> Option<u32> {
    None
}

/// `ntpath.expanduser`: `%USERPROFILE%` (else `%HOMEDRIVE%%HOMEPATH%`), and
/// for another user's name the sibling directory of that home.
fn windows_home(user: &str) -> Option<String> {
    let home = match std::env::var("USERPROFILE") {
        Ok(profile) => profile,
        Err(_) => {
            let path = std::env::var("HOMEPATH").ok()?;
            std::env::var("HOMEDRIVE").unwrap_or_default() + &path
        }
    };
    if user.is_empty() {
        return Some(home);
    }
    let home_path = Path::new(&home);
    if home_path.file_name().is_some_and(|name| name == user) {
        return Some(home);
    }
    let parent = home_path.parent().unwrap_or(Path::new(""));
    Some(parent.join(user).to_string_lossy().into_owned())
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
