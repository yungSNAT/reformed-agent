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
    let eof = format!("x\n{SKILLS_BLOCK_START}          items: [{{ a: 1 }}],\n        }}");
    assert_eq!(rewrite_sidebar(&eof).unwrap(), format!("x\n{SKILLS_BLOCK}"));
    assert!(rewrite_sidebar("no block").is_err());
}

#[test]
fn relative_links_and_catalog_rows_match_source_layout() {
    let meta = Meta {
        bundled: false,
        category: "cat".into(),
        sub: Some("sub".into()),
        slug: "slug".into(),
        rel_path: "cat/sub/slug".into(),
    };
    let link = Regex::new(r"\[([^\]]+)\]\(([^)]+)\)").unwrap();
    let scheme = Regex::new(r"\A[a-z]+://").unwrap();
    assert_eq!(
        rewrite_relative_links(
            "[a](./r.md) [b](s/x) [c](https://x) [d](#h) [e](/p) [f](mailto:a@b)",
            &meta,
            &link,
            &scheme,
        ),
        "[a](https://github.com/NousResearch/hermes-agent/blob/main/optional-skills/cat/sub/slug/r.md) [b](https://github.com/NousResearch/hermes-agent/blob/main/optional-skills/cat/sub/slug/s/x) [c](https://x) [d](#h) [e](/p) [f](mailto:a@b)"
    );
    assert_eq!(
        meta.href(),
        "/docs/user-guide/skills/optional/cat/cat-sub-slug"
    );

    let skill = Skill {
        meta: Meta {
            bundled: true,
            category: "dev".into(),
            sub: None,
            slug: "lint".into(),
            rel_path: "dev/lint".into(),
        },
        fm: Py::Dict(vec![("description".into(), Py::Str(" a | b\nc ".into()))]),
        body: String::new(),
    };
    let text = catalog(
        "H\n\n",
        "| S | D | P |\n|---|---|---|",
        catalog_rows(&[skill], true, &tag()),
        "",
    );
    assert_eq!(
        text,
        "H\n\n## dev\n\n| S | D | P |\n|---|---|---|\n| [`lint`](/docs/user-guide/skills/bundled/dev/dev-lint) | a \\| b c | `dev/lint` |\n"
    );
}
