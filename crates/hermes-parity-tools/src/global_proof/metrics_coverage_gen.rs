use super::*;

fn load(text: &str) -> Py {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("report.json"), text).unwrap();
    match read_json(dir.path(), "report.json") {
        Ok(value) => value,
        Err(_) => panic!("failed to parse test json"),
    }
}

fn sample() -> Py {
    load(
        r#"{
            "a": 3,
            "b": 2.5,
            "c": true,
            "d": 1.9,
            "e": "abc",
            "f": [1],
            "g": [{"x": 1}],
            "h": {},
            "i": "",
            "j": "x",
            "k": {"pass": true},
            "l": {"pass": 0},
            "n": "7",
            "rows": [{"path": "a"}, {"path": "b"}, {"other": 1}]
        }"#,
    )
}

#[test]
fn value_defaults_to_zero_and_reads_numbers() {
    let report = sample();
    assert_eq!(value(&report, "missing"), Ok(0.0));
    assert_eq!(value(&report, "a"), Ok(3.0));
    assert_eq!(value(&report, "b"), Ok(2.5));
}

#[test]
fn count_handles_all_variants() {
    let report = sample();
    assert_eq!(count(&report, "missing"), Ok(0.0));
    assert_eq!(count(&report, "a"), Ok(3.0));
    assert_eq!(count(&report, "c"), Ok(1.0));
    assert_eq!(count(&report, "d"), Ok(1.0));
    assert_eq!(count(&report, "n"), Ok(7.0));
    assert!(count(&report, "e").is_err());
    let err = count(&report, "f").unwrap_err();
    assert!(err.contains("f must be convertible to an integer"));
}

#[test]
fn required_and_required_number() {
    let report = sample();
    assert!(required(&report, "a").is_ok());
    assert_eq!(
        required(&report, "zz").err(),
        Some("report is missing zz".to_owned())
    );
    assert_eq!(required_number(&report, "a"), Ok(3.0));
    assert!(required_number(&report, "zz").is_err());
}

#[test]
fn row_count_read_variants() {
    let report = sample();
    assert!(matches!(
        RowCount::read(&report, "a"),
        Ok(RowCount::Integer(3))
    ));
    assert!(matches!(
        RowCount::read(&report, "c"),
        Ok(RowCount::Integer(1))
    ));
    assert!(matches!(
        RowCount::read(&report, "b"),
        Ok(RowCount::Float(v)) if v == 2.5
    ));
    match RowCount::read(&report, "e") {
        Err(err) => assert_eq!(err, "e must be numeric"),
        Ok(_) => panic!("expected error"),
    }
    assert!(RowCount::read(&report, "missing").is_err());
}

#[test]
fn row_count_subtract_and_as_float() {
    let result = RowCount::Integer(5).subtract(RowCount::Integer(2));
    assert!(matches!(result, Ok(RowCount::Integer(3))));
    match RowCount::Integer(i128::MIN).subtract(RowCount::Integer(1)) {
        Err(err) => assert_eq!(err, "coverage count exceeds the integer range"),
        Ok(_) => panic!("expected overflow error"),
    }
    let mixed = RowCount::Integer(5).subtract(RowCount::Float(1.5));
    assert!(matches!(mixed, Ok(RowCount::Float(v)) if v == 3.5));
    assert_eq!(RowCount::Integer(4).as_float(), 4.0);
    assert_eq!(RowCount::Float(0.25).as_float(), 0.25);
}

#[test]
fn required_list_variants() {
    let report = sample();
    assert_eq!(required_list(&report, "f").map(<[Py]>::len), Ok(1));
    assert_eq!(
        required_list(&report, "a").err(),
        Some("a must contain a list".to_owned())
    );
}

#[test]
fn evidence_rows_variants() {
    let report = sample();
    assert_eq!(evidence_rows(&report, "missing").map(<[Py]>::len), Ok(0));
    assert_eq!(evidence_rows(&report, "g").map(<[Py]>::len), Ok(1));
    assert_eq!(evidence_rows(&report, "h").map(<[Py]>::len), Ok(0));
    assert_eq!(evidence_rows(&report, "i").map(<[Py]>::len), Ok(0));
    assert_eq!(
        evidence_rows(&report, "f").err(),
        Some("f must contain a list of evidence objects".to_owned())
    );
    assert!(evidence_rows(&report, "j").is_err());
}

#[test]
fn required_text_variants() {
    let report = sample();
    assert_eq!(required_text(&report, "j"), Ok("x"));
    assert_eq!(
        required_text(&report, "a").err(),
        Some("a must contain text".to_owned())
    );
}

#[test]
fn is_text_matches_only_equal_strings() {
    let text = Py::Str("x".into());
    let other = Py::Int(1);
    assert!(is_text(Some(&text), "x"));
    assert!(!is_text(Some(&text), "y"));
    assert!(!is_text(Some(&other), "x"));
    assert!(!is_text(None, "x"));
}

#[test]
fn passed_reads_pass_flag() {
    let report = sample();
    assert!(passed(&report, "k"));
    assert!(!passed(&report, "l"));
    assert!(!passed(&report, "missing"));
}

#[test]
fn path_set_collects_paths() {
    let report = sample();
    let rows = required_list(&report, "rows").unwrap();
    let set = path_set(rows);
    let expected: BTreeSet<String> = ["", "a", "b"].iter().map(|s| (*s).to_owned()).collect();
    assert_eq!(set, expected);
}
