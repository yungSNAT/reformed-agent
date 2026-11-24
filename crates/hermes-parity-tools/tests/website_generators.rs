//! Parity cases for the docs site generators: each output must match, byte
//! for byte, what the retired Python scripts wrote for the same fixture tree
//! (`tests/fixtures/website/expected`), apart from the extraction timestamp
//! and the absolute repository path, which are masked.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/website")
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

/// A copy of the fixture tree, prepared for `mode`, and its resolved path.
fn tree(mode: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("run");
    copy_tree(&fixture().join("tree"), &root);
    match mode {
        "unified" => {
            fs::create_dir_all(root.join("website/static/api")).unwrap();
            fs::copy(
                fixture().join("unified-index.json"),
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
    fs::read_to_string(fixture().join("expected").join(name)).unwrap()
}

/// The repository root as the tool prints it, replaced by `<ROOT>`.
fn mask_root(text: &str, root: &Path) -> String {
    let shown = root.display().to_string();
    let shown = shown.strip_prefix(r"\\?\").unwrap_or(&shown).replace('\\', "/");
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
        assert!(meta.lines().nth(1).unwrap().starts_with("  \"extractedAt\": \"20"));
        assert_eq!(
            mask_extracted_at(&meta),
            mask_extracted_at(&expected(&format!("extract-{mode}.meta.json"))),
            "{mode} skills-meta.json"
        );
    }
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
