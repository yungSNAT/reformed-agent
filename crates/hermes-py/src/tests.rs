use super::*;

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
fn json_string_preserves_lone_surrogates() {
    assert_eq!(
        json_utf16_string([0x2f, 0xdcff, 0xd83d, 0xde00, 0x22]),
        r#""/\udcff\ud83d\ude00\"""#,
    );
}

#[test]
fn strings_repr_escapes_what_python_cannot_print() {
    // repr("a\xa0b\u200bc\u2028d\U000e0001e\ue000f\x85g\xadh\u0378i \u00e9\U0001F600\u3000j")
    // in Python 3.15 (Unicode 17).
    assert_eq!(
        repr(&Py::from(
            "a\u{a0}b\u{200b}c\u{2028}d\u{e0001}e\u{e000}f\u{85}g\u{ad}h\u{378}i \u{e9}\u{1F600}\u{3000}j"
        )),
        r"'a\xa0b\u200bc\u2028d\U000e0001e\ue000f\x85g\xadh\u0378i é😀\u3000j'"
    );
    // ASCII controls, as before.
    assert_eq!(repr(&Py::from("\u{1}\u{7f}\t")), r"'\x01\x7f\t'");
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
    let str_of = |text: &str| str_of(&loads(text).unwrap());
    assert_eq!(str_of("null"), "None");
    assert_eq!(str_of("true"), "True");
    assert_eq!(str_of("7"), "7");
    assert_eq!(str_of("7.0"), "7.0");
    assert_eq!(str_of(r#""x""#), "x");
    assert_eq!(str_of(r#"["a", 1, null]"#), "['a', 1, None]");
    assert_eq!(str_of(r#"["it's"]"#), "[\"it's\"]");
    assert_eq!(str_of(r#"{"k": false}"#), "{'k': False}");
    assert_eq!(
        str_of(r#"{"z": 1, "a": [NaN, -Infinity]}"#),
        "{'z': 1, 'a': [nan, -inf]}"
    );
}

#[test]
fn loads_reads_documents_as_python_does() {
    // json.loads: a repeated key keeps its first place and last value; big
    // integers stay integers; escapes and surrogate pairs decode.
    assert_eq!(
        loads(r#"{"b": 1, "a": 2, "b": 3}"#).unwrap(),
        Py::dict([("b", Py::Int(3)), ("a", Py::Int(2))])
    );
    assert_eq!(loads("18446744073709551616").unwrap(), Py::Int(1 << 64));
    assert_eq!(loads("-0.5e1").unwrap(), Py::Float(-5.0));
    assert_eq!(
        loads(r#""a\u00e9\ud83d\ude00\n""#).unwrap(),
        Py::from("a\u{e9}\u{1f600}\n")
    );
    for bad in ["", "01", "1.", "[1,]", "{'a': 1}", r#""a"#, "tru", "[1] x"] {
        assert!(loads(bad).is_err(), "{bad}");
    }
}

#[test]
fn loads_fails_closed_past_1000_levels_of_nesting() {
    // An error at this port's own bound, never a stack overflow.
    let arrays = |depth: usize| format!("{}{}", "[".repeat(depth), "]".repeat(depth));
    let objects = |depth: usize| format!("{}1{}", r#"{"a":"#.repeat(depth), "}".repeat(depth));
    assert!(loads(&arrays(1_000)).is_ok());
    assert!(loads(&objects(1_000)).is_ok());
    for deep in [
        arrays(1_001),
        objects(1_001),
        arrays(1_000_000),
        objects(1_000_000),
    ] {
        let err = loads(&deep).unwrap_err();
        assert!(err.starts_with("Maximum nesting depth exceeded"), "{err}");
    }
}

#[test]
fn strip_matches_python_whitespace() {
    assert_eq!(strip("\u{1c}\t a b \u{a0}\u{3000}\n"), "a b");
    assert_eq!(strip("\u{200b}x"), "\u{200b}x");
}

#[test]
fn isoformat_drops_zero_microseconds_as_python_does() {
    let whole: jiff::Timestamp = "2026-11-17T01:02:03Z".parse().unwrap();
    assert_eq!(isoformat(whole), "2026-11-17T01:02:03+00:00");
    let fractional = whole + jiff::SignedDuration::from_micros(450);
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

#[test]
fn expanduser_leaves_a_path_without_a_tilde() {
    assert_eq!(expanduser("docs/~x").unwrap(), PathBuf::from("docs/~x"));
}

#[cfg(unix)]
#[test]
fn expanduser_reads_a_named_users_home() {
    // pathlib.Path("~root/x").expanduser() and an unknown user's RuntimeError.
    assert_eq!(expanduser("~root/x").unwrap(), PathBuf::from("/root/x"));
    assert!(expanduser("~no-such-user-w6/x").is_err());
}

#[cfg(windows)]
#[test]
fn expanduser_puts_another_user_beside_this_home() {
    // ntpath.expanduser(r"~someone-else\x") with USERPROFILE set.
    let home = PathBuf::from(std::env::var("USERPROFILE").expect("USERPROFILE"));
    let expected = home
        .parent()
        .expect("profile parent")
        .join("someone-else")
        .join("x");
    assert_eq!(expanduser(r"~someone-else\x").unwrap(), expected);
}
