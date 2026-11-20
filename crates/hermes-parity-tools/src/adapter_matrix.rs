//! Generate the gateway platform and memory plugin feature matrix.

use std::fmt::Write as _;
use std::fs;
use std::path::{Component, Path, PathBuf};

use clap::Args;
use regex::Regex;

use crate::py::{self, Py};

#[derive(Args, Debug)]
pub struct AdapterMatrixArgs {
    /// Repository root path
    #[arg(long, default_value = ".")]
    repo_root: String,
    /// Output JSON path relative to the repository root
    #[arg(long, default_value = "docs/parity/adapter-feature-matrix.json")]
    out_json: String,
    /// Output Markdown path relative to the repository root
    #[arg(long, default_value = "docs/parity/adapter-feature-matrix.md")]
    out_md: String,
}

struct Row<'a> {
    category: &'static str,
    name: &'a str,
    feature: &'a str,
}

impl Row<'_> {
    fn json(&self) -> Py {
        Py::dict([
            ("category", self.category.into()),
            ("feature", self.feature.into()),
            ("name", self.name.into()),
            ("status", "rust-native".into()),
        ])
    }
}

fn read_source(path: &Path) -> Result<String, String> {
    fs::read_to_string(path)
        .map(|source| source.replace("\r\n", "\n").replace('\r', "\n"))
        .map_err(|error| format!("{}: {error}", path.display()))
}

fn resolve_argument(path: &Path) -> Result<PathBuf, String> {
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

fn write_report(path: &Path, contents: String) -> Result<(), String> {
    let contents = if cfg!(windows) {
        contents.replace('\n', "\r\n")
    } else {
        contents
    };
    fs::write(path, contents).map_err(|error| format!("{}: {error}", path.display()))
}

pub fn run(args: AdapterMatrixArgs) -> Result<i32, String> {
    let root = resolve_argument(Path::new(&args.repo_root))?;
    let platforms = read_source(&root.join("crates/hermes-gateway/src/platforms/mod.rs"))?;
    let memory = read_source(&root.join("crates/hermes-agent/src/memory_plugins/mod.rs"))?;
    // Match the original generator's patterns, including Python's whitespace
    // controls beyond the Unicode White_Space class.
    let platform_modules =
        Regex::new(r#"#\[cfg\(feature = "([^"]+)"\)\][\s\x1c-\x1f]*pub mod ([a-zA-Z0-9_]+);"#)
            .expect("valid platform module pattern");
    let memory_modules =
        Regex::new(r"(?m)^pub mod ([a-zA-Z0-9_]+);").expect("valid memory module pattern");
    let mut rows: Vec<_> = platform_modules
        .captures_iter(&platforms)
        .map(|found| {
            let (_, [feature, name]) = found.extract();
            Row {
                category: "platform_adapter",
                name,
                feature,
            }
        })
        .collect();
    let platform_count = rows.len();
    rows.extend(memory_modules.captures_iter(&memory).map(|found| {
        let (_, [name]) = found.extract();
        Row {
            category: "memory_plugin",
            name,
            feature: "always",
        }
    }));
    let memory_count = rows.len() - platform_count;
    rows.sort_by_key(|row| (row.category, row.name));

    let generated_at = py::utc_isoformat();
    let report = Py::dict([
        ("generated_at_utc", generated_at.as_str().into()),
        (
            "summary",
            Py::dict([
                ("platform_adapters", Py::Int(platform_count as i128)),
                ("memory_plugins", Py::Int(memory_count as i128)),
                ("non_rust_native", Py::Int(0)),
                ("placeholder_status_entries", Py::Int(0)),
            ]),
        ),
        ("items", Py::List(rows.iter().map(Row::json).collect())),
    ]);
    let mut markdown = format!(
        "# Adapter Feature Matrix\n\nGenerated: `{generated_at}`\n\n| Category | Name | Feature Flag | Status |\n| --- | --- | --- | --- |\n"
    );
    for row in &rows {
        writeln!(
            markdown,
            "| {} | `{}` | `{}` | rust-native |",
            row.category, row.name, row.feature
        )
        .expect("writing to a String cannot fail");
    }
    writeln!(
        markdown,
        "\n- Platform adapters: `{platform_count}`, memory plugins: `{memory_count}`."
    )
    .expect("writing to a String cannot fail");

    let out_json = resolve_argument(&py::join(&root, &args.out_json))?;
    let out_md = resolve_argument(&py::join(&root, &args.out_md))?;
    for path in [&out_json, &out_md] {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
        }
    }
    write_report(&out_json, py::dumps(&report, Some(2), true) + "\n")?;
    write_report(&out_md, markdown)?;
    println!("Wrote {}", py::path_str(&out_json));
    println!("Wrote {}", py::path_str(&out_md));
    Ok(0)
}
