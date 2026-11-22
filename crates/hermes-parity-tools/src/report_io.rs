use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::py::{self, Py};

pub(crate) fn truth(value: &Py) -> bool {
    match value {
        Py::Null => false,
        Py::Bool(value) => *value,
        Py::Int(value) => *value != 0,
        Py::Float(value) => *value != 0.0,
        Py::Str(value) => !value.is_empty(),
        Py::List(value) => !value.is_empty(),
        Py::Dict(value) => !value.is_empty(),
    }
}

pub(crate) fn resolve_argument(path: &Path) -> Result<PathBuf, String> {
    let path = if path.as_os_str().is_empty() {
        Path::new(".")
    } else {
        path
    };
    let absolute = std::path::absolute(path).map_err(|error| error.to_string())?;
    let resolved = follow_links(&absolute, 40)?;
    Ok(py::resolve(&resolved))
}

fn follow_links(path: &Path, remaining_links: usize) -> Result<PathBuf, String> {
    let mut resolved = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                resolved.pop();
            }
            Component::Normal(name) => {
                let candidate = resolved.join(name);
                if let Ok(target) = fs::read_link(&candidate) {
                    let remaining = remaining_links.checked_sub(1).ok_or_else(|| {
                        format!("{}: too many symbolic links", candidate.display())
                    })?;
                    // Read the link itself even if its target directory has
                    // not been created yet.
                    resolved = follow_links(&resolved.join(target), remaining)?;
                } else {
                    resolved = candidate;
                }
            }
            _ => resolved.push(part),
        }
    }
    Ok(resolved)
}

pub(crate) fn write_report(path: &Path, contents: String) -> Result<(), String> {
    let contents = if cfg!(windows) {
        contents.replace('\n', "\r\n")
    } else {
        contents
    };
    fs::write(path, contents).map_err(|error| format!("{}: {error}", path.display()))
}

pub(crate) fn write_reports(
    root: &Path,
    out_json: &str,
    out_md: &str,
    json: String,
    markdown: String,
) -> Result<(), String> {
    let out_json = resolve_argument(&py::join(root, out_json))?;
    let out_md = resolve_argument(&py::join(root, out_md))?;
    for path in [&out_json, &out_md] {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
        }
    }
    write_report(&out_json, json)?;
    write_report(&out_md, markdown)?;
    println!("Wrote {}", py::path_str(&out_json));
    println!("Wrote {}", py::path_str(&out_md));
    Ok(())
}

pub(crate) fn write_coverage_reports(
    root: &Path,
    out_json: &str,
    out_md: &str,
    json: String,
    markdown: String,
) -> Result<(), String> {
    let out_json = resolve_argument(&py::join(root, out_json))?;
    let out_md = resolve_argument(&py::join(root, out_md))?;
    for (path, contents) in [(&out_json, json), (&out_md, markdown)] {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
        }
        write_report(path, contents)?;
    }
    println!("Wrote {}", py::path_str(&out_json));
    println!("Wrote {}", py::path_str(&out_md));
    Ok(())
}
