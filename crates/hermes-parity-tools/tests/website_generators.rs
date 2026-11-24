//! Parity cases for the docs site generators: each output must match, byte
//! for byte, what the retired Python scripts wrote for the same fixture tree
//! (`tests/fixtures/{website,skill-docs}/expected`), apart from the extraction
//! timestamp, absolute repository path, and generated-page note.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const OLD_SKILL_NOTE: &str = "by website/scripts/generate-skill-docs.py.";
const NEW_SKILL_NOTE: &str = "by `cargo run -p hermes-parity-tools -- generate-skill-docs`.";

fn fixture(suite: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(suite)
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).unwrap();
        }
    }
}

fn temp_tree(suite: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("run");
    copy_tree(&fixture(suite).join("tree"), &root);
    (dir, root)
}

/// A copy of the fixture tree, prepared for `mode`, and its resolved path.
fn tree(mode: &str) -> (tempfile::TempDir, PathBuf) {
    let (dir, root) = temp_tree("website");
    let site = fixture("website");
    match mode {
        "unified" => {
            fs::create_dir_all(root.join("website/static/api")).unwrap();
            fs::copy(
                site.join("unified-index.json"),
                root.join("website/static/api/skills-index.json"),
            )
            .unwrap();
        }
        "localonly" => fs::remove_dir_all(root.join("skills/index-cache")).unwrap(),
        _ => {}
    }
    let root = fs::canonicalize(&root).unwrap();
    (dir, root)
}

fn run(root: &Path, command: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_hermes-parity-tools"))
        .args([command, "--repo-root"])
        .arg(root)
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        output.status.success(),
        "{command} failed\nstdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    stdout
}

fn expected(name: &str) -> String {
    fs::read_to_string(fixture("website").join("expected").join(name)).unwrap()
}

/// The repository root as the tool prints it, replaced by `<ROOT>`.
fn mask_root(text: &str, root: &Path) -> String {
    let shown = root.display().to_string();
    let shown = shown
        .strip_prefix(r"\\?\")
        .unwrap_or(&shown)
        .replace('\\', "/");
    text.replace('\\', "/").replace(&shown, "<ROOT>")
}

fn mask_extracted_at(text: &str) -> String {
    text.lines()
        .map(|line| {
            if line.starts_with("  \"extractedAt\": ") {
                "  \"extractedAt\": \"<TIME>\","
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn extract_skills_matches_the_python_generator_in_every_index_mode() {
    for mode in ["unified", "legacy", "localonly"] {
        let (_dir, root) = tree(mode);
        let stdout = run(&root, "extract-skills");
        assert_eq!(
            mask_root(&stdout, &root),
            expected(&format!("extract-{mode}.stdout")),
            "{mode} stdout"
        );
        let data = root.join("website/src/data");
        assert_eq!(
            fs::read_to_string(data.join("skills.json")).unwrap(),
            expected(&format!("extract-{mode}.skills.json")),
            "{mode} skills.json"
        );
        let meta = fs::read_to_string(data.join("skills-meta.json")).unwrap();
        assert!(meta
            .lines()
            .nth(1)
            .unwrap()
            .starts_with("  \"extractedAt\": \"20"));
        assert_eq!(
            mask_extracted_at(&meta),
            mask_extracted_at(&expected(&format!("extract-{mode}.meta.json"))),
            "{mode} skills-meta.json"
        );
    }
}

#[test]
fn generate_llms_txt_without_a_docs_tree_writes_the_header_only() {
    let (_dir, root) = tree("legacy");
    fs::remove_dir_all(root.join("website/docs")).unwrap();
    let stdout = run(&root, "generate-llms-txt");
    assert_eq!(mask_root(&stdout, &root), expected("llms-nodocs.stdout"));
    let stat = root.join("website/static");
    assert_eq!(
        fs::read_to_string(stat.join("llms.txt")).unwrap(),
        expected("llms-nodocs.txt")
    );
    assert_eq!(
        fs::read_to_string(stat.join("llms-full.txt")).unwrap(),
        expected("llms-full-nodocs.txt")
    );
}

#[test]
fn generate_llms_txt_matches_the_python_generator() {
    let (_dir, root) = tree("legacy");
    let stdout = run(&root, "generate-llms-txt");
    assert_eq!(mask_root(&stdout, &root), expected("llms.stdout"));
    let stat = root.join("website/static");
    assert_eq!(
        fs::read_to_string(stat.join("llms.txt")).unwrap(),
        expected("llms.txt")
    );
    assert_eq!(
        fs::read_to_string(stat.join("llms-full.txt")).unwrap(),
        expected("llms-full.txt")
    );
}

fn files(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files(root, &path, out);
        } else {
            out.push(path.strip_prefix(root).unwrap().to_path_buf());
        }
    }
}

#[test]
fn generate_skill_docs_matches_the_python_generator() {
    let (_dir, root) = temp_tree("skill-docs");
    fs::create_dir_all(root.join("website/docs/reference")).unwrap();
    let root = fs::canonicalize(&root).unwrap();
    let stdout = run(&root, "generate-skill-docs");
    let expected_root = fixture("skill-docs").join("expected");
    assert_eq!(
        mask_root(&stdout, &root),
        fs::read_to_string(expected_root.join("stdout")).unwrap()
    );

    let mut expected = Vec::new();
    files(
        &expected_root,
        &expected_root.join("website"),
        &mut expected,
    );
    let mut written = Vec::new();
    files(&root, &root.join("website"), &mut written);
    expected.sort();
    written.sort();
    assert_eq!(written, expected);
    for rel in expected {
        let want = fs::read_to_string(expected_root.join(&rel))
            .unwrap()
            .replace(OLD_SKILL_NOTE, NEW_SKILL_NOTE)
            .replace(
                "regenerated by `website/scripts/generate-skill-docs.py`.",
                "regenerated by `cargo run -p hermes-parity-tools -- generate-skill-docs`.",
            );
        let got = fs::read_to_string(root.join(&rel)).unwrap();
        assert_eq!(got, want, "{}", rel.display());
    }
}
