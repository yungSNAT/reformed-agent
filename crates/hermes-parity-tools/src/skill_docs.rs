//! `generate-skill-docs`: writes one docs page per skill under
//! `website/docs/user-guide/skills/<bundled|optional>/<category>/`, the 2
//! catalog pages under `website/docs/reference/`, and the Skills block of
//! `website/sidebars.ts`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use regex::{Captures, Regex};

use crate::py::{self, Py};
use crate::report_io::resolve_argument;
use crate::website::{
    load_yaml, lstrip, read_text, rel_posix, rstrip, split_frontmatter, take_chars, title, truthy,
    write_text, WebsiteArgs,
};

const GENERATED_NOTE: &str = "{/* This page is auto-generated from the skill's SKILL.md by `cargo run -p hermes-parity-tools -- generate-skill-docs`. Edit the source SKILL.md, not this page. */}";

const BOX_DRAWING: &str = "┌┐└┘─│═║╔╗╚╝╠╣╦╩╬├┤┬┴┼╭╮╯╰▶◀▲▼";

const SAFE_TAGS: [&str; 40] = [
    "br",
    "hr",
    "img",
    "a",
    "b",
    "i",
    "em",
    "strong",
    "code",
    "kbd",
    "sup",
    "sub",
    "span",
    "div",
    "p",
    "ul",
    "ol",
    "li",
    "table",
    "thead",
    "tbody",
    "tr",
    "td",
    "th",
    "details",
    "summary",
    "blockquote",
    "pre",
    "mark",
    "small",
    "u",
    "s",
    "del",
    "ins",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
];

struct Meta {
    bundled: bool,
    category: String,
    sub: Option<String>,
    slug: String,
    rel_path: String,
}

impl Meta {
    fn kind(&self) -> &'static str {
        if self.bundled {
            "bundled"
        } else {
            "optional"
        }
    }

    fn source_dir(&self) -> &'static str {
        if self.bundled {
            "skills"
        } else {
            "optional-skills"
        }
    }

    fn page_id(&self) -> String {
        match &self.sub {
            Some(sub) => format!("{}-{sub}-{}", self.category, self.slug),
            None => format!("{}-{}", self.category, self.slug),
        }
    }

    fn href(&self) -> String {
        format!(
            "/docs/user-guide/skills/{}/{}/{}",
            self.kind(),
            self.category,
            self.page_id()
        )
    }
}

struct Skill {
    meta: Meta,
    fm: Py,
    body: String,
}

impl Skill {
    fn name(&self) -> String {
        self.fm
            .get("name")
            .map_or_else(|| self.meta.slug.clone(), py::str_of)
    }

    fn fm_str(&self, key: &str) -> String {
        self.fm
            .get(key)
            .filter(|v| truthy(v))
            .map(py::str_of)
            .unwrap_or_default()
    }
}

/// Python's `re.sub(r"\s+", " ", s).strip()` with `str.isspace` whitespace.
fn collapse_space(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_space = false;
    for c in s.chars() {
        if c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c) {
            if !in_space {
                out.push(' ');
            }
            in_space = true;
        } else {
            out.push(c);
            in_space = false;
        }
    }
    py::strip(&out).to_string()
}

fn sanitize_yaml_string(s: &str) -> String {
    collapse_space(&s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn wrap_ascii_art(code: &str) -> String {
    if code.chars().any(|c| BOX_DRAWING.contains(c)) {
        format!("<!-- ascii-guard-ignore -->\n{code}\n<!-- ascii-guard-ignore-end -->")
    } else {
        code.to_string()
    }
}

fn escape_text(text: &str, tag: &Regex) -> String {
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        let rest = &text[i..];
        let c = rest.chars().next().expect("in bounds");
        match c {
            '`' => {
                let run_len = rest.len() - rest.trim_start_matches('`').len();
                let run = &rest[..run_len];
                match text[i + run_len..].find(run) {
                    Some(at) => {
                        let end = i + run_len + at + run_len;
                        out.push_str(&text[i..end]);
                        i = end;
                    }
                    None => {
                        out.push_str(rest);
                        i = text.len();
                    }
                }
                continue;
            }
            '{' => out.push_str("&#123;"),
            '}' => out.push_str("&#125;"),
            '<' => {
                if rest.starts_with("<!--") {
                    if let Some(at) = rest.find("-->") {
                        out.push_str(&rest[..at + 3]);
                        i += at + 3;
                        continue;
                    }
                }
                if let Some(m) = tag.captures(rest) {
                    if SAFE_TAGS.contains(&m[2].to_lowercase().as_str()) {
                        out.push_str(&m[0]);
                        i += m[0].len();
                        continue;
                    }
                }
                out.push_str("&lt;");
            }
            other => out.push(other),
        }
        i += c.len_utf8();
    }
    out
}

/// Escapes MDX metacharacters outside fenced code blocks and wraps fenced
/// blocks holding box-drawing characters in ascii-guard ignore markers.
fn mdx_escape_body(body: &str, tag: &Regex) -> String {
    let mut segments: Vec<(bool, String)> = Vec::new();
    let mut buf: Vec<&str> = Vec::new();
    let mut fence: Option<String> = None;
    for line in body.split('\n') {
        let stripped = lstrip(line);
        match &fence {
            None if stripped.starts_with("```") || stripped.starts_with("~~~") => {
                if !buf.is_empty() {
                    segments.push((false, buf.join("\n")));
                    buf.clear();
                }
                buf.push(line);
                let marker = stripped.chars().next().expect("fence");
                let len = stripped.len() - stripped.trim_start_matches(marker).len();
                fence = Some(stripped[..len].to_string());
            }
            None => buf.push(line),
            Some(marker) => {
                buf.push(line);
                if stripped.starts_with(marker.as_str()) {
                    segments.push((true, buf.join("\n")));
                    buf.clear();
                    fence = None;
                }
            }
        }
    }
    if !buf.is_empty() {
        segments.push((fence.is_some(), buf.join("\n")));
    }
    segments
        .iter()
        .map(|(code, text)| {
            if *code {
                wrap_ascii_art(text)
            } else {
                escape_text(text, tag)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn rewrite_relative_links(body: &str, meta: &Meta, link: &Regex, scheme: &Regex) -> String {
    let base = format!(
        "https://github.com/NousResearch/hermes-agent/blob/main/{}/{}",
        meta.source_dir(),
        meta.rel_path
    );
    link.replace_all(body, |m: &Captures| {
        let url = py::strip(&m[2]);
        if scheme.is_match(url)
            || url.starts_with('#')
            || url.starts_with('/')
            || url.starts_with("mailto:")
        {
            return m[0].to_string();
        }
        let clean = url.strip_prefix("./").unwrap_or(url);
        format!("[{}]({base}/{clean})", &m[1])
    })
    .into_owned()
}

struct Patterns {
    tag: Regex,
    link: Regex,
    scheme: Regex,
}

/// The items Python's `for x in value` yields, as strings: a list's items or
/// a string's characters.
fn items(value: &Py) -> Vec<String> {
    match value {
        Py::List(items) => items.iter().map(py::str_of).collect(),
        Py::Str(s) => s.chars().map(String::from).collect(),
        other => vec![py::str_of(other)],
    }
}

fn ticked(value: &Py) -> String {
    items(value)
        .iter()
        .map(|s| format!("`{s}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn render_page(skill: &Skill, index: &BTreeMap<String, &Meta>, p: &Patterns) -> String {
    let meta = &skill.meta;
    let fm = &skill.fm;
    let name = skill.name();
    let description = py::strip(&skill.fm_str("description")).to_string();
    let mut short = if description.is_empty() {
        name.clone()
    } else {
        py::strip(description.split('.').next().unwrap_or_default()).to_string()
    };
    if short.chars().count() > 160 {
        short = format!("{}...", take_chars(&short, 157));
    }
    let display = title(&name.replace(['-', '_'], " "));
    let hermes = fm
        .get("metadata")
        .filter(|m| truthy(m))
        .and_then(|m| m.get("hermes"))
        .filter(|h| truthy(h));
    let field = |key: &str| hermes.and_then(|h| h.get(key)).filter(|v| truthy(v));

    let mut rows: Vec<(&str, String)> = Vec::new();
    rows.push((
        "Source",
        if meta.bundled {
            "Bundled (installed by default)".to_string()
        } else {
            format!(
                "Optional — install with `hermes skills install official/{}/{}`",
                meta.category, meta.slug
            )
        },
    ));
    rows.push(("Path", format!("`{}/{}`", meta.source_dir(), meta.rel_path)));
    let present = |key: &str| fm.get(key).filter(|v| truthy(v));
    if let Some(version) = present("version") {
        rows.push(("Version", format!("`{}`", py::str_of(version))));
    }
    if let Some(author) = present("author") {
        rows.push(("Author", py::str_of(author)));
    }
    if let Some(license) = present("license") {
        rows.push(("License", py::str_of(license)));
    }
    if let Some(deps) = present("dependencies") {
        let shown = match deps {
            Py::List(_) => ticked(deps),
            other => format!("`{}`", py::str_of(other)),
        };
        rows.push(("Dependencies", shown));
    }
    if let Some(platforms) = present("platforms") {
        let shown = match platforms {
            Py::List(_) => items(platforms).join(", "),
            other => py::str_of(other),
        };
        rows.push(("Platforms", shown));
    }
    if let Some(tags) = field("tags") {
        rows.push(("Tags", ticked(tags)));
    }
    if let Some(related) = field("related_skills") {
        let links: Vec<String> = items(related)
            .into_iter()
            .map(|r| match index.get(&r) {
                Some(target) => format!("[`{r}`]({})", target.href()),
                None => format!("`{r}`"),
            })
            .collect();
        rows.push(("Related skills", links.join(", ")));
    }
    let info = rows
        .iter()
        .map(|(k, v)| format!("| {k} | {v} |"))
        .collect::<Vec<_>>()
        .join("\n");

    let short_or_name = if short.is_empty() { &name } else { &short };
    let mut fm_title = sanitize_yaml_string(&format!("{display} — {short_or_name}"));
    if fm_title.chars().count() > 120 {
        fm_title = sanitize_yaml_string(&display);
    }
    let desc_source = [&short, &description, &name]
        .into_iter()
        .find(|s| !s.is_empty())
        .cloned()
        .unwrap_or_default();
    let fm_desc = sanitize_yaml_string(&desc_source);
    let label = sanitize_yaml_string(&display);
    let body = rewrite_relative_links(py::strip(&skill.body), meta, &p.link, &p.scheme);
    let body = mdx_escape_body(&body, &p.tag);
    let description = mdx_escape_body(&description, &p.tag);
    format!(
        "---\ntitle: \"{fm_title}\"\nsidebar_label: \"{label}\"\ndescription: \"{fm_desc}\"\n---\n\n\
         {GENERATED_NOTE}\n\n# {display}\n\n{description}\n\n## Skill metadata\n\n\
         | | |\n|---|---|\n{info}\n\n## Reference: full SKILL.md\n\n:::info\n\
         The following is the complete skill definition that Hermes loads when this skill is triggered. \
         This is what the agent sees as instructions when the skill is active.\n:::\n\n{body}\n"
    )
}

fn find_skill_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in fs::read_dir(dir).map_err(|error| format!("{}: {error}", dir.display()))? {
        let entry = entry.map_err(|error| format!("{}: {error}", dir.display()))?;
        let path = entry.path();
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            find_skill_files(&path, out)?;
        } else if entry.file_name() == "SKILL.md" {
            out.push(path);
        }
    }
    Ok(())
}

fn discover(root: &Path) -> Result<Vec<Skill>, String> {
    let mut skills = Vec::new();
    for (bundled, dir) in [(true, "skills"), (false, "optional-skills")] {
        let source = root.join(dir);
        let mut files = Vec::new();
        if source.is_dir() {
            find_skill_files(&source, &mut files)?;
        }
        files.sort();
        for file in files {
            let parent = file.parent().unwrap_or(&source);
            let rel_path = rel_posix(parent, &source);
            let parts: Vec<String> = rel_path.split('/').map(str::to_string).collect();
            let (category, sub, slug) = match parts.as_slice() {
                [one] => (one.clone(), None, one.clone()),
                [category, slug] => (category.clone(), None, slug.clone()),
                [category, sub, slug] => (category.clone(), Some(sub.clone()), slug.clone()),
                _ => return Err(format!("Unexpected skill layout: {}", file.display())),
            };
            let text = read_text(&file)?;
            let (fm_text, body) = split_frontmatter(&text)
                .ok_or_else(|| format!("{}: no frontmatter", file.display()))?;
            let fm = if py::strip(fm_text).is_empty() {
                Py::Dict(Vec::new())
            } else {
                load_yaml(fm_text).ok_or_else(|| format!("{}: YAML error", file.display()))?
            };
            let fm = if truthy(&fm) {
                fm
            } else {
                Py::Dict(Vec::new())
            };
            skills.push(Skill {
                meta: Meta {
                    bundled,
                    category,
                    sub,
                    slug,
                    rel_path,
                },
                fm,
                body: body.trim_start_matches('\n').to_string(),
            });
        }
    }
    Ok(skills)
}

fn catalog_rows(skills: &[Skill], bundled: bool, tag: &Regex) -> BTreeMap<String, Vec<String>> {
    let mut by_cat: BTreeMap<String, Vec<&Skill>> = BTreeMap::new();
    for skill in skills.iter().filter(|s| s.meta.bundled == bundled) {
        by_cat
            .entry(skill.meta.category.clone())
            .or_default()
            .push(skill);
    }
    by_cat
        .into_iter()
        .map(|(category, mut items)| {
            items.sort_by(|a, b| a.meta.slug.cmp(&b.meta.slug));
            let rows = items
                .iter()
                .map(|skill| {
                    let name = skill.name();
                    let mut desc = py::strip(&skill.fm_str("description")).to_string();
                    if desc.chars().count() > 240 {
                        desc = format!("{}...", rstrip(take_chars(&desc, 237)));
                    }
                    let desc = mdx_escape_body(&desc, tag)
                        .replace('|', "\\|")
                        .replace('\n', " ");
                    let href = skill.meta.href();
                    if bundled {
                        format!(
                            "| [`{name}`]({href}) | {desc} | `{}` |",
                            skill.meta.rel_path
                        )
                    } else {
                        format!("| [**{name}**]({href}) | {desc} |")
                    }
                })
                .collect();
            (category, rows)
        })
        .collect()
}

fn catalog(head: &str, header: &str, rows: BTreeMap<String, Vec<String>>, tail: &str) -> String {
    let mut out = head.to_string();
    for (category, lines) in rows {
        out.push_str(&format!("## {category}\n\n{header}\n"));
        for line in lines {
            out.push_str(&line);
            out.push('\n');
        }
        out.push('\n');
    }
    out.push_str(tail);
    format!("{}\n", rstrip(&out))
}

const BUNDLED_HEAD: &str = include_str!("skill_docs/bundled-head.md");
const OPTIONAL_HEAD: &str = include_str!("skill_docs/optional-head.md");
const OPTIONAL_TAIL: &str = include_str!("skill_docs/optional-tail.md");

const SKILLS_BLOCK_START: &str =
    "        {\n          type: 'category',\n          label: 'Skills',\n";
const SKILLS_BLOCK: &str = "        {\n          type: 'category',\n          label: 'Skills',\n          collapsed: true,\n          items: [\n            'reference/skills-catalog',\n            'reference/optional-skills-catalog',\n          ],\n        },\n";

/// Replaces the Skills category in `sidebars.ts` with the 2 catalog links.
fn rewrite_sidebar(text: &str) -> Result<String, String> {
    let start = text
        .find(SKILLS_BLOCK_START)
        .ok_or("Could not find Skills sidebar block to replace")?;
    let mut depth = 0i32;
    for (offset, c) in text[start..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    let close = start + offset;
                    let end = text[close..].find('\n').map_or(0, |n| close + n + 1);
                    return Ok(format!("{}{SKILLS_BLOCK}{}", &text[..start], &text[end..]));
                }
            }
            _ => {}
        }
    }
    Err("Could not find end of Skills sidebar block".to_string())
}

pub fn run(args: WebsiteArgs) -> Result<i32, String> {
    let root = resolve_argument(Path::new(&args.repo_root))?;
    let docs = root.join("website/docs");
    let pages = docs.join("user-guide/skills");
    let skills = discover(&root)?;
    println!("Discovered {} skills", skills.len());

    let mut index: BTreeMap<String, &Meta> = BTreeMap::new();
    for skill in &skills {
        let name = skill.name();
        if !index.contains_key(&name) || skill.meta.bundled {
            index.insert(name, &skill.meta);
        }
    }
    let patterns = Patterns {
        tag: Regex::new(r"\A<(/?)([A-Za-z][A-Za-z0-9]*)([^<>]*)>").expect("valid pattern"),
        link: Regex::new(r"\[([^\]]+)\]\(([^)]+)\)").expect("valid pattern"),
        scheme: Regex::new(r"\A[a-z]+://").expect("valid pattern"),
    };
    for skill in &skills {
        let dir = pages.join(skill.meta.kind()).join(&skill.meta.category);
        fs::create_dir_all(&dir).map_err(|error| format!("{}: {error}", dir.display()))?;
        let path = dir.join(format!("{}.md", skill.meta.page_id()));
        write_text(&path, &render_page(skill, &index, &patterns))?;
    }
    println!(
        "Wrote {} per-skill pages under {}",
        skills.len(),
        py::path_str(&pages)
    );

    let bundled = catalog(
        BUNDLED_HEAD,
        "| Skill | Description | Path |\n|-------|-------------|------|",
        catalog_rows(&skills, true, &patterns.tag),
        "",
    );
    write_text(&docs.join("reference/skills-catalog.md"), &bundled)?;
    println!("Updated reference/skills-catalog.md");
    let optional = catalog(
        OPTIONAL_HEAD,
        "| Skill | Description |\n|-------|-------------|",
        catalog_rows(&skills, false, &patterns.tag),
        OPTIONAL_TAIL,
    );
    write_text(
        &docs.join("reference/optional-skills-catalog.md"),
        &optional,
    )?;
    println!("Updated reference/optional-skills-catalog.md");

    let sidebar = root.join("website/sidebars.ts");
    write_text(&sidebar, &rewrite_sidebar(&read_text(&sidebar)?)?)?;
    println!("Updated sidebar: {}", py::path_str(&sidebar));
    Ok(0)
}

#[cfg(test)]
#[path = "skill_docs_tests.rs"]
mod tests;
