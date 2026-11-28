use super::*;

#[test]
fn title_matches_python_str_title() {
    assert_eq!(title("note taking"), "Note Taking");
    assert_eq!(title("3d printing"), "3D Printing");
    assert_eq!(title("mIXED cASE x402"), "Mixed Case X402");
}

#[test]
fn split_frontmatter_matches_python_split_with_maxsplit_two() {
    assert_eq!(
        split_frontmatter("---\na: 1\n---\nbody"),
        Some(("\na: 1\n", "\nbody"))
    );
    assert_eq!(
        split_frontmatter("---\na: b---c\n---\n"),
        Some(("\na: b", "c\n---\n"))
    );
    assert_eq!(split_frontmatter("--- only"), None);
    assert_eq!(split_frontmatter("no marker"), None);
}

#[test]
fn overview_truncates_by_characters_at_a_sentence_boundary() {
    let long = "é".repeat(300) + ". " + &"b".repeat(300);
    assert_eq!(extract_overview(&long), "é".repeat(300) + ".");
    let unbroken = "c".repeat(600);
    assert_eq!(extract_overview(&unbroken), "c".repeat(500) + "…");
}

#[test]
fn overview_takes_body_lines_from_a_heading_paragraph() {
    assert_eq!(extract_overview("# Title\nBody line\n\nNext"), "Body line");
    assert_eq!(extract_overview("# Only\n\n:::tip\nx\n:::\n\nReal"), "Real");
}

#[test]
fn yaml_values_keep_python_types() {
    let fm = load_yaml("version: 1.0\ncount: 3\nname: x\nlist: [a, 2]").unwrap();
    assert_eq!(fm.get("version"), Some(&Py::Float(1.0)));
    assert_eq!(fm.get("count"), Some(&Py::Int(3)));
    assert_eq!(
        py::dumps(fm.get("list").unwrap(), None, false),
        "[\"a\", 2]"
    );
}

#[test]
fn yaml_metadata_preserves_missing_flags_and_signed_counts() {
    let yaml = load_yaml(
        "enabled: true\narchived: false\ndescription: null\nbalance: -7\nnotes: [null, false, -1]",
    )
    .unwrap();

    assert_eq!(yaml.get("enabled"), Some(&Py::Bool(true)));
    assert_eq!(yaml.get("archived"), Some(&Py::Bool(false)));
    assert_eq!(yaml.get("description"), Some(&Py::Null));
    assert_eq!(yaml.get("balance"), Some(&Py::Int(-7)));
    assert_eq!(
        yaml.get("notes"),
        Some(&Py::List(vec![Py::Null, Py::Bool(false), Py::Int(-1)]))
    );
    assert_eq!(yaml.get("absent"), None);
}

#[test]
fn yaml_supports_large_integers_beyond_u64() {
    let fm = load_yaml("pos: 99999999999999999999\nneg: -99999999999999999999").unwrap();
    assert_eq!(fm.get("pos"), Some(&Py::Int(99_999_999_999_999_999_999)));
    assert_eq!(fm.get("neg"), Some(&Py::Int(-99_999_999_999_999_999_999)));
}

#[test]
fn yaml_merge_keys_follow_pyyaml_precedence() {
    let fm =
        load_yaml("a: &a\n  x: 1\n  y: 1\nb: &b\n  y: 2\n  z: 2\n<<: [*a, *b]\nx: explicit\nw: 0")
            .unwrap();
    assert_eq!(fm.get("x"), Some(&Py::Str("explicit".into())));
    assert_eq!(fm.get("y"), Some(&Py::Int(1)));
    assert_eq!(fm.get("z"), Some(&Py::Int(2)));
    assert_eq!(fm.get("w"), Some(&Py::Int(0)));
    assert_eq!(fm.get("<<"), None);

    // Explicit key before << still wins over the merged value.
    let before = load_yaml("base: &b\n  author: anchored\nauthor: explicit\n<<: *b").unwrap();
    assert_eq!(before.get("author"), Some(&Py::Str("explicit".into())));

    // Nested merge inside a merged source and multiple << keys (later << overrides earlier <<).
    let nested =
        load_yaml("a: &a\n  x: 1\n  y: 1\nb: &b\n  <<: *a\n  y: 2\nc: &c\n  y: 3\n<<: *b\n<<: *c")
            .unwrap();
    assert_eq!(nested.get("x"), Some(&Py::Int(1)));
    assert_eq!(nested.get("y"), Some(&Py::Int(3)));

    // Non-mapping merge target is rejected, matching PyYAML ConstructorError.
    assert_eq!(load_yaml("<<: 123"), None);
    assert_eq!(load_yaml("<<: [123]"), None);
}

#[test]
fn yaml_scalars_follow_yaml_1_2_core_schema() {
    let fm = load_yaml("a: yes\nb: on\nc: 010\nd: 1_000\ne: 1:30\nf: 1e3").unwrap();
    assert_eq!(fm.get("a"), Some(&Py::Str("yes".into())));
    assert_eq!(fm.get("b"), Some(&Py::Str("on".into())));
    assert_eq!(fm.get("c"), Some(&Py::Str("010".into())));
    assert_eq!(fm.get("d"), Some(&Py::Str("1_000".into())));
    assert_eq!(fm.get("e"), Some(&Py::Str("1:30".into())));
    assert_eq!(fm.get("f"), Some(&Py::Float(1000.0)));
}
