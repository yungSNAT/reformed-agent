use super::*;

fn tag() -> Regex {
    Regex::new(r"\A<(/?)([A-Za-z][A-Za-z0-9]*)([^<>]*)>").unwrap()
}

#[test]
fn escape_leaves_inline_code_comments_and_safe_tags() {
    let text = "a {b} `{c}` <br/> <x> <!-- k --> <!-- open";
    assert_eq!(
        escape_text(text, &tag()),
        "a &#123;b&#125; `{c}` <br/> &lt;x> <!-- k --> &lt;!-- open"
    );
}

#[test]
fn fenced_blocks_are_untouched_and_box_art_is_wrapped() {
    let body = "x {y}\n```\n{z}\n```\n~~~\n│\n~~~\nend";
    assert_eq!(
        mdx_escape_body(body, &tag()),
        "x &#123;y&#125;\n```\n{z}\n```\n<!-- ascii-guard-ignore -->\n~~~\n│\n~~~\n<!-- ascii-guard-ignore-end -->\nend"
    );
}

#[test]
fn yaml_strings_are_escaped_and_whitespace_collapsed() {
    assert_eq!(
        sanitize_yaml_string(" a \"b\"\n\t\\c "),
        "a \\\"b\\\" \\\\c"
    );
}

#[test]
fn sidebar_skills_block_is_replaced_by_the_catalog_links() {
    let text = format!("x\n{SKILLS_BLOCK_START}          items: [{{ a: 1 }}],\n        }},\ny\n");
    assert_eq!(
        rewrite_sidebar(&text).unwrap(),
        format!("x\n{SKILLS_BLOCK}y\n")
    );
    assert!(rewrite_sidebar("no block").is_err());
}
