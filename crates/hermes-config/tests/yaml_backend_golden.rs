//! Pins what the YAML backend reads and writes for config files: values,
//! typed fields, parse errors and the bytes each config writer produces.
//! A change of YAML crate must leave every expected file below unchanged.

use std::path::{Path, PathBuf};

use hermes_config::{
    atomic_yaml_write, load_user_config_file, save_config_yaml, set_user_config_value,
};
use serde::Deserialize;

const CORPUS: &str = include_str!("fixtures/yaml_backend/corpus.yaml");
const USER_CONFIG: &str = include_str!("fixtures/yaml_backend/user-config.yaml");

fn example_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cli-config.yaml.example")
}

fn corpus_value() -> serde_yaml::Value {
    serde_yaml::from_str(CORPUS).expect("corpus parses")
}

fn value_round_trip() -> String {
    serde_yaml::to_string(&corpus_value()).expect("corpus emits")
}

/// Plain scalars read into typed fields, as config structs read them.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct TypedCorpus {
    leading_zero: String,
    octal: u32,
    hex: u32,
    version_like: String,
    date_like: String,
    yes_word: String,
    on_word: String,
    big_unsigned: u64,
    exponent: f64,
    null_tilde: Option<String>,
    null_empty: Option<String>,
    env_ref: String,
    windows_path: String,
    literal_block: String,
    folded_block: String,
    keep_trailing: String,
    strip_trailing: String,
    multiline_quoted: String,
}

fn typed_read() -> String {
    let typed: Result<TypedCorpus, _> = serde_yaml::from_str(CORPUS);
    match typed {
        Ok(typed) => format!("{typed:#?}\n"),
        Err(err) => format!("error: {err}\n"),
    }
}

fn parse_errors() -> String {
    let deep = format!("{}{}", "[".repeat(200), "]".repeat(200));
    let docs = [
        "duplicate: 1\nduplicate: 2\n",
        "first: 1\n---\nsecond: 2\n",
        "a:\n  b: 1\n c: 2\n",
        "a: 'unclosed\n",
        "a:\n\tb: 1\n",
        "a: *missing\n",
        "too_big: 18446744073709551616\n",
        "key: [1, 2\n",
        deep.as_str(),
    ];
    let mut out = String::new();
    for doc in docs {
        match serde_yaml::from_str::<serde_yaml::Value>(doc) {
            Ok(_) => out.push_str("ok\n"),
            Err(err) => out.push_str(&format!("error: {err}\n")),
        }
    }
    out
}

fn atomic_write() -> String {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("written.yaml");
    atomic_yaml_write(&path, &corpus_value(), Some("# trailer\n")).expect("atomic write");
    std::fs::read_to_string(&path).expect("read written")
}

fn example_config_saved() -> String {
    let config = load_user_config_file(&example_config_path()).expect("example config loads");
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.yaml");
    save_config_yaml(&path, &config).expect("save config");
    std::fs::read_to_string(&path).expect("read saved")
}

fn user_config_after_set() -> String {
    let home = tempfile::tempdir().expect("home");
    let path = home.path().join("config.yaml");
    std::fs::write(&path, USER_CONFIG).expect("write user config");
    let mut out = String::new();
    for (key, value) in [
        ("display.skin", "mono"),
        ("max_turns", "90"),
        ("fallback_models.1", "openrouter:openai/gpt-5"),
        ("terminal.cwd", "~/work: main"),
        ("future_upstream_section.nested_flag", "false"),
    ] {
        if let Err(err) = set_user_config_value(home.path(), key, value) {
            out.push_str(&format!("set {key} failed: {err}\n"));
        }
    }
    out.push_str(&std::fs::read_to_string(&path).expect("read config"));
    out
}

fn bad_config_error() -> String {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.yaml");
    std::fs::write(&path, "max_turns: lots\n").expect("write config");
    match load_user_config_file(&path) {
        Ok(_) => "ok\n".to_string(),
        Err(err) => format!("error: {err}\n"),
    }
}

#[test]
fn value_round_trip_is_unchanged() {
    assert_eq!(
        value_round_trip(),
        include_str!("fixtures/yaml_backend/value-round-trip.expected")
    );
}

#[test]
fn typed_read_is_unchanged() {
    assert_eq!(
        typed_read(),
        include_str!("fixtures/yaml_backend/typed-read.expected")
    );
}

#[test]
fn parse_errors_are_unchanged() {
    assert_eq!(
        parse_errors(),
        include_str!("fixtures/yaml_backend/parse-errors.expected")
    );
}

#[test]
fn atomic_yaml_write_is_unchanged() {
    assert_eq!(
        atomic_write(),
        include_str!("fixtures/yaml_backend/atomic-write.expected")
    );
}

/// Compared as values: the typed config keeps some maps in a `HashMap`, so
/// their key order changes from run to run.
#[test]
fn example_config_save_is_unchanged() {
    let saved: serde_yaml::Value =
        serde_yaml::from_str(&example_config_saved()).expect("saved config parses");
    let expected: serde_yaml::Value = serde_yaml::from_str(include_str!(
        "fixtures/yaml_backend/example-config-saved.expected"
    ))
    .expect("expected config parses");
    assert_eq!(saved, expected);
}

#[test]
fn config_set_is_unchanged() {
    assert_eq!(
        user_config_after_set(),
        include_str!("fixtures/yaml_backend/user-config-after-set.expected")
    );
}

#[test]
fn bad_config_error_is_unchanged() {
    assert_eq!(
        bad_config_error(),
        include_str!("fixtures/yaml_backend/bad-config-error.expected")
    );
}
