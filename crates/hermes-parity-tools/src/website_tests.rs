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
