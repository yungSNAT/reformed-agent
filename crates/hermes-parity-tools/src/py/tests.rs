use super::*;
use serde_json::json;

#[test]
fn dumps_matches_python_indent_2() {
    // json.dumps({"b": [1, 2.5, []], "a": {}, "c": "é\"\n"}, indent=2)
    let value = Py::dict([
        (
            "b",
            Py::List(vec![Py::Int(1), Py::Float(2.5), Py::List(vec![])]),
        ),
        ("a", Py::Dict(vec![])),
        ("c", Py::from("é\"\n")),
    ]);
    assert_eq!(
        dumps(&value, Some(2), false),
        "{\n  \"b\": [\n    1,\n    2.5,\n    []\n  ],\n  \"a\": {},\n  \"c\": \"\\u00e9\\\"\\n\"\n}"
    );
    assert_eq!(
        dumps(&value, None, true),
        "{\"a\": {}, \"b\": [1, 2.5, []], \"c\": \"\\u00e9\\\"\\n\"}"
    );
}

#[test]
fn strings_escape_as_python_ensure_ascii_does() {
    // json.dumps("\x7f\x01\U0001F600\b\f\\")
    assert_eq!(
        dumps(&Py::from("\u{7f}\u{1}\u{1F600}\u{8}\u{c}\\"), None, false),
        "\"\\u007f\\u0001\\ud83d\\ude00\\b\\f\\\\\""
    );
}

#[test]
fn floats_print_as_python_repr() {
    let cases = [
        (1.0, "1.0"),
        (0.5, "0.5"),
        (1272.0 / 3094.0, "0.41111829347123463"),
        (0.0001, "0.0001"),
        (0.00001, "1e-05"),
        (1.5e16, "1.5e+16"),
        (1e16, "1e+16"),
        (123456789012345.6, "123456789012345.6"),
        (-0.0, "-0.0"),
        (-2.25, "-2.25"),
        (100.0, "100.0"),
    ];
    for (value, expected) in cases {
        assert_eq!(float_repr(value), expected, "{value}");
    }
    assert_eq!(dumps(&Py::Float(f64::NAN), None, false), "NaN");
    assert_eq!(dumps(&Py::Float(f64::INFINITY), None, false), "Infinity");
}

#[test]
fn str_of_follows_python_str() {
    assert_eq!(str_of(&json!(null)), "None");
    assert_eq!(str_of(&json!(true)), "True");
    assert_eq!(str_of(&json!(7)), "7");
    assert_eq!(str_of(&json!(7.0)), "7.0");
    assert_eq!(str_of(&json!("x")), "x");
    assert_eq!(str_of(&json!(["a", 1, null])), "['a', 1, None]");
    assert_eq!(str_of(&json!(["it's"])), "[\"it's\"]");
    assert_eq!(str_of(&json!({"k": false})), "{'k': False}");
}

#[test]
fn strip_matches_python_whitespace() {
    assert_eq!(strip("\u{1c}\t a b \u{a0}\u{3000}\n"), "a b");
    assert_eq!(strip("\u{200b}x"), "\u{200b}x");
}

#[test]
fn isoformat_drops_zero_microseconds_as_python_does() {
    use chrono::TimeZone;
    let whole = chrono::Utc.with_ymd_and_hms(2026, 11, 17, 1, 2, 3).unwrap();
    assert_eq!(isoformat(whole), "2026-11-17T01:02:03+00:00");
    let fractional = whole + chrono::Duration::microseconds(450);
    assert_eq!(isoformat(fractional), "2026-11-17T01:02:03.000450+00:00");
}

#[test]
fn set_replaces_in_place_and_appends_new_keys() {
    let mut value = Py::dict([("a", Py::Int(1)), ("b", Py::Int(2))]);
    value.set("a", Py::Int(3));
    value.set("c", Py::Int(4));
    assert_eq!(dumps(&value, None, false), "{\"a\": 3, \"b\": 2, \"c\": 4}");
}

#[test]
fn resolve_keeps_a_missing_tail() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("not-yet").join("report.json");
    let resolved = resolve(&missing);
    assert!(resolved.ends_with("not-yet/report.json"));
    assert!(resolved.is_absolute());
    assert!(!path_str(&resolved).starts_with(r"\\?\"));
}

#[test]
fn join_uses_the_platform_separator_as_pathlib_does() {
    let base = Path::new("root");
    let joined = join(base, "docs/parity//x.json");
    let expected: PathBuf = ["root", "docs", "parity", "x.json"].iter().collect();
    assert_eq!(joined, expected);
}
