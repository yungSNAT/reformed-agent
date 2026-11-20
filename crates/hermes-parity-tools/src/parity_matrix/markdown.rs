use std::fmt::Write as _;

use crate::py::{self, Py};

fn field<'a>(row: &'a Py, name: &str) -> &'a Py {
    row.get(name).expect("field belongs to a matrix report")
}

fn label(row: &Py, name: &str) -> String {
    py::str_of(field(row, name))
}

fn rows<'a>(report: &'a Py, name: &str) -> &'a [Py] {
    let Py::List(values) = field(report, name) else {
        unreachable!("matrix report table is a list");
    };
    values
}

pub(super) fn render(report: &Py, top: &str) -> String {
    let refs = field(report, "refs");
    let diverged = *field(refs, "merge_base") == Py::Null;
    let merge_base = if diverged {
        "none (history divergence)".to_owned()
    } else {
        label(refs, "merge_base")
    };
    let mut output = format!(
        "# Parity Matrix\n\nGenerated: `{}`\n\n## Scope\n\n- Local ref: `{}` (`{}`)\n- Upstream ref: `{}` (`{}`)\n- Merge base: `{merge_base}`\n\n## Summary\n\n| Metric | Value |\n| --- | ---: |\n",
        label(report, "generated_at_utc"),
        label(refs, "local_ref"),
        label(refs, "local_sha"),
        label(refs, "upstream_ref"),
        label(refs, "upstream_sha"),
    );
    let summary = field(report, "summary");
    for (name, key) in [
        (
            "Commits behind local (`upstream` ancestry only)",
            "commits_behind",
        ),
        (
            "Commits ahead local (`local` ancestry only)",
            "commits_ahead",
        ),
        (
            "Upstream commits missing by patch-id (`git cherry local upstream`, `+`)",
            "upstream_patch_missing",
        ),
        (
            "Upstream commits represented by patch-id (`git cherry local upstream`, `-`)",
            "upstream_patch_represented",
        ),
        (
            "Local commits unique by patch-id (`git cherry upstream local`, `+`)",
            "local_patch_unique",
        ),
        ("Files only in upstream tree", "files_only_upstream"),
        ("Files only in local tree", "files_only_local"),
        ("Shared files identical content", "files_shared_identical"),
        ("Shared files different content", "files_shared_different"),
        (
            "Total files changed (`local` vs `upstream`)",
            "tree_files_changed",
        ),
        ("Insertions (`local` vs `upstream`)", "tree_insertions"),
        ("Deletions (`local` vs `upstream`)", "tree_deletions"),
    ] {
        writeln!(output, "| {name} | {} |", label(summary, key)).unwrap();
    }

    for (title, key) in [
        ("upstream-only", "top_upstream_only"),
        ("shared-different", "top_shared_different"),
        ("local-only", "top_local_only"),
    ] {
        writeln!(
            output,
            "\n## Top {top} {title} buckets\n\n| Bucket | Files |\n| --- | ---: |"
        )
        .unwrap();
        let buckets = rows(report, key);
        for row in buckets {
            writeln!(
                output,
                "| `{}` | {} |",
                label(row, "path"),
                label(row, "count")
            )
            .unwrap();
        }
        if buckets.is_empty() {
            output.push_str("| _(none)_ | 0 |\n");
        }
    }

    output.push_str("\n## Workstream Routing\n\n| Workstream | Issue | Name | Upstream-only | Shared-different | Risk | Effort |\n| --- | ---: | --- | ---: | ---: | --- | --- |\n");
    for row in rows(report, "workstream_matrix") {
        writeln!(
            output,
            "| `{}` | #{} | {} | {} | {} | {} | {} |",
            label(row, "workstream"),
            label(row, "issue"),
            label(row, "name"),
            label(row, "upstream_only"),
            label(row, "shared_different"),
            label(row, "risk"),
            label(row, "effort"),
        )
        .unwrap();
    }

    output.push_str("\n## Commit Mapping\n\n");
    let commits = field(report, "commit_mapping");
    for (name, key) in [
        ("Upstream missing by patch-id", "upstream_patch_missing"),
        (
            "Upstream represented by patch-id",
            "upstream_patch_represented",
        ),
        ("Local unique by patch-id", "local_patch_unique"),
    ] {
        writeln!(output, "- {name}: `{}`", label(commits, key)).unwrap();
    }
    writeln!(
        output,
        "- Intentional divergence tracked items: `{}` (covered files: `{}`)",
        label(commits, "intentional_divergence_items"),
        label(commits, "intentional_divergence_covered_files")
    )
    .unwrap();
    if diverged {
        output.push_str("- Merge base is absent; patch-id mapping is used as primary commit equivalence signal.\n");
    }
    let divergences = rows(report, "intentional_divergence");
    if !divergences.is_empty() {
        output.push_str("\n## Intentional Divergence Registry\n\n| ID | Status | Workstream | Matched Files | Summary |\n| --- | --- | --- | ---: | --- |\n");
        for row in divergences {
            writeln!(
                output,
                "| `{}` | {} | {} | {} | {} |",
                label(row, "id"),
                label(row, "status"),
                label(row, "workstream"),
                label(row, "matched_files"),
                label(row, "summary")
            )
            .unwrap();
        }
    }
    output.push_str("\n\n## Notes\n\n- Data is computed directly from git refs in this repository.\n- Tree-level parity classification does not require a merge-base.\n- Commit representation/missing uses patch-id equivalence from `git cherry`.\n- Default behavior fetches the configured upstream branch into `upstream/main`.\n");
    output
}
