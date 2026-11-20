//! Generate the gateway platform and memory plugin feature matrix.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use clap::Args;
use regex::Regex;

use crate::py::{self, Py};
use crate::report_io::{resolve_argument, write_reports};

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

    write_reports(
        &root,
        &args.out_json,
        &args.out_md,
        py::dumps(&report, Some(2), true) + "\n",
        markdown,
    )?;
    Ok(0)
}
