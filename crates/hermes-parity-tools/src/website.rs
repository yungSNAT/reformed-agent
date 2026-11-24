//! The documentation site's build-time generators: `extract-skills` writes
//! the Skills Hub data and `generate-llms-txt` writes `llms.txt` and
//! `llms-full.txt`. Both run from `website/scripts/prebuild.mjs`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use clap::Args;

use crate::py::{self, Py};
use crate::report_io::resolve_argument;
pub(crate) use crate::report_io::truth as truthy;

#[derive(Args, Debug)]
pub struct WebsiteArgs {
    /// Repository root path
    #[arg(long, default_value = ".")]
    pub(crate) repo_root: String,
}

// ---- Python string helpers ------------------------------------------------

fn is_py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

pub(crate) fn rstrip(s: &str) -> &str {
    s.trim_end_matches(is_py_space)
}

pub(crate) fn lstrip(s: &str) -> &str {
    s.trim_start_matches(is_py_space)
}

/// Python's `str.title()`: a cased character after an uncased one is upper
/// case, every other cased character lower case.
pub(crate) fn title(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_cased = false;
    for c in s.chars() {
        let cased = c.is_lowercase() || c.is_uppercase();
        if cased && !prev_cased {
            out.extend(c.to_uppercase());
        } else if cased {
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
        prev_cased = cased;
    }
    out
}

/// The first `n` characters of `s`, as Python's `s[:n]`.
pub(crate) fn take_chars(s: &str, n: usize) -> &str {
    s.char_indices().nth(n).map_or(s, |(i, _)| &s[..i])
}

/// `path.read_text(encoding="utf-8")` with universal newlines.
pub(crate) fn read_text(path: &Path) -> Result<String, String> {
    fs::read_to_string(path)
        .map(|text| text.replace("\r\n", "\n").replace('\r', "\n"))
        .map_err(|error| format!("{}: {error}", path.display()))
}

pub(crate) fn write_text(path: &Path, text: &str) -> Result<(), String> {
    fs::write(path, text).map_err(|error| format!("{}: {error}", path.display()))
}

/// `yaml.safe_load` of a frontmatter block, as the values the generators read.
/// A repeated mapping key keeps its last value and `<<` merges mappings (explicit
/// keys win; earlier entries in a merge list win), as PyYAML does (quoted `"<<"`
/// keys also merge, and quoted `"<<": <non-mapping>` drops the skill like
/// unquoted `<<`). Integers outside `i128::MIN..=i128::MAX` coerce to `f64`
/// (`Py::Float`). Plain scalars resolve by the YAML 1.2 core schema, not
/// PyYAML's YAML 1.1 rules (`yes`, `on`, `010`, `1_000`, `1:30` and `1e3` differ);
/// no skill in the tree uses such a scalar where the generators read it.
pub(crate) fn load_yaml(text: &str) -> Option<Py> {
    serde_yaml::from_str::<YamlPy>(text).ok().map(|v| v.0)
}

struct YamlPy(Py);

impl<'de> serde::Deserialize<'de> for YamlPy {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(YamlVisitor).map(YamlPy)
    }
}

struct YamlVisitor;

impl<'de> serde::de::Visitor<'de> for YamlVisitor {
    type Value = Py;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a YAML value")
    }
    fn visit_unit<E>(self) -> Result<Py, E> {
        Ok(Py::Null)
    }
    fn visit_none<E>(self) -> Result<Py, E> {
        Ok(Py::Null)
    }
    fn visit_bool<E>(self, v: bool) -> Result<Py, E> {
        Ok(Py::Bool(v))
    }
    fn visit_i64<E>(self, v: i64) -> Result<Py, E> {
        Ok(Py::Int(i128::from(v)))
    }
    fn visit_u64<E>(self, v: u64) -> Result<Py, E> {
        Ok(Py::Int(i128::from(v)))
    }
    fn visit_i128<E>(self, v: i128) -> Result<Py, E> {
        Ok(Py::Int(v))
    }
    fn visit_u128<E>(self, v: u128) -> Result<Py, E> {
        Ok(i128::try_from(v).map_or_else(|_| Py::Float(v as f64), Py::Int))
    }
    fn visit_f64<E>(self, v: f64) -> Result<Py, E> {
        Ok(Py::Float(v))
    }
    fn visit_str<E>(self, v: &str) -> Result<Py, E> {
        Ok(Py::Str(v.to_string()))
    }
    fn visit_string<E>(self, v: String) -> Result<Py, E> {
        Ok(Py::Str(v))
    }
    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Py, A::Error> {
        let mut items = Vec::new();
        while let Some(YamlPy(item)) = seq.next_element()? {
            items.push(item);
        }
        Ok(Py::List(items))
    }
    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Py, A::Error> {
        let mut merged = Vec::new();
        let mut explicit = Vec::new();
        while let Some((YamlPy(key), YamlPy(value))) = map.next_entry()? {
            if matches!(&key, Py::Str(s) if s == "<<") {
                match value {
                    Py::Dict(pairs) => merged.extend(pairs),
                    Py::List(items) => {
                        if !items.iter().all(Py::is_dict) {
                            return Err(serde::de::Error::custom(
                                "expected mapping in YAML merge sequence",
                            ));
                        }
                        for item in items.into_iter().rev() {
                            if let Py::Dict(pairs) = item {
                                merged.extend(pairs);
                            }
                        }
                    }
                    _ => {
                        return Err(serde::de::Error::custom(
                            "expected mapping or sequence of mappings for YAML merge",
                        ));
                    }
                }
                continue;
            }
            let key = match key {
                Py::Str(s) => s,
                Py::Bool(b) => b.to_string(),
                Py::Null => "null".to_string(),
                other => py::str_of(&other),
            };
            explicit.push((key, value));
        }
        let mut dict = Py::Dict(Vec::new());
        for (k, v) in merged {
            dict.set(&k, v);
        }
        for (k, v) in explicit {
            dict.set(&k, v);
        }
        Ok(dict)
    }
}

/// Splits `---\n<yaml>---<body>` as `text.split("---", 2)` does; `None` when
/// the text does not start with `---` or has no closing marker.
pub(crate) fn split_frontmatter(text: &str) -> Option<(&str, &str)> {
    let rest = text.strip_prefix("---")?;
    let end = rest.find("---")?;
    Some((&rest[..end], &rest[end + 3..]))
}

fn get<'a>(value: &'a Py, key: &str) -> Option<&'a Py> {
    value.get(key)
}

fn get_or(value: &Py, key: &str, default: Py) -> Py {
    get(value, key).cloned().unwrap_or(default)
}

fn as_str(value: &Py) -> &str {
    match value {
        Py::Str(s) => s,
        _ => "",
    }
}

// ---- extract-skills -------------------------------------------------------

const CATEGORY_LABELS: [(&str, &str); 30] = [
    ("apple", "Apple"),
    ("autonomous-ai-agents", "AI Agents"),
    ("blockchain", "Blockchain"),
    ("communication", "Communication"),
    ("creative", "Creative"),
    ("data-science", "Data Science"),
    ("devops", "DevOps"),
    ("dogfood", "Dogfood"),
    ("domain", "Domain"),
    ("email", "Email"),
    ("gaming", "Gaming"),
    ("gifs", "GIFs"),
    ("github", "GitHub"),
    ("health", "Health"),
    ("inference-sh", "Inference"),
    ("leisure", "Leisure"),
    ("mcp", "MCP"),
    ("media", "Media"),
    ("migration", "Migration"),
    ("mlops", "MLOps"),
    ("note-taking", "Note-Taking"),
    ("productivity", "Productivity"),
    ("red-teaming", "Red Teaming"),
    ("research", "Research"),
    ("security", "Security"),
    ("smart-home", "Smart Home"),
    ("social-media", "Social Media"),
    ("software-development", "Software Dev"),
    ("translation", "Translation"),
    ("other", "Other"),
];

const SOURCE_LABELS: [(&str, &str); 4] = [
    ("anthropics_skills", "Anthropic"),
    ("openai_skills", "OpenAI"),
    ("claude_marketplace", "Claude Marketplace"),
    ("lobehub", "LobeHub"),
];

const UNIFIED_SOURCE_LABELS: [(&str, &str); 9] = [
    ("official", "optional"),
    ("skills.sh", "skills.sh"),
    ("skills-sh", "skills.sh"),
    ("clawhub", "ClawHub"),
    ("browse-sh", "browse.sh"),
    ("lobehub", "LobeHub"),
    ("claude-marketplace", "Claude Marketplace"),
    ("well-known", "Well-Known"),
    ("github", "GitHub"),
];

const GITHUB_TAP_LABELS: [(&str, &str); 6] = [
    ("openai/skills", "OpenAI"),
    ("anthropics/skills", "Anthropic"),
    ("huggingface/skills", "HuggingFace"),
    ("VoltAgent/awesome-agent-skills", "VoltAgent"),
    ("garrytan/gstack", "gstack"),
    ("MiniMax-AI/cli", "MiniMax"),
];

const TAG_CATEGORIES: [(&str, &[&str]); 13] = [
    (
        "software-development",
        &[
            "programming",
            "code",
            "coding",
            "software-development",
            "frontend-development",
            "backend-development",
            "web-development",
            "react",
            "python",
            "typescript",
            "java",
            "rust",
        ],
    ),
    (
        "creative",
        &["writing", "design", "creative", "art", "image-generation"],
    ),
    ("research", &["education", "academic", "research"]),
    ("social-media", &["marketing", "seo", "social-media"]),
    ("productivity", &["productivity", "business"]),
    ("data-science", &["data", "data-science"]),
    ("mlops", &["machine-learning", "deep-learning"]),
    ("devops", &["devops"]),
    ("gaming", &["gaming", "game", "game-development"]),
    ("media", &["music", "media", "video"]),
    ("health", &["health", "fitness"]),
    ("translation", &["translation", "language-learning"]),
    ("security", &["security", "cybersecurity"]),
];

const MIN_CATEGORY_SIZE: usize = 4;

fn lookup(table: &[(&'static str, &'static str)], key: &str) -> Option<&'static str> {
    table.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

fn category_label(category: &str) -> String {
    lookup(&CATEGORY_LABELS, category)
        .map_or_else(|| title(&category.replace('-', " ")), str::to_string)
}

fn extract_overview(body: &str) -> String {
    let paragraphs = body
        .split("\n\n")
        .map(py::strip)
        .filter(|p| !p.is_empty())
        .take(6);
    for p in paragraphs {
        let mut p = p.to_string();
        if p.starts_with('#') {
            let lines: Vec<&str> = p
                .split('\n')
                .filter(|ln| !py::strip(ln).is_empty() && !lstrip(ln).starts_with('#'))
                .collect();
            if lines.is_empty() {
                continue;
            }
            p = py::strip(&lines.join("\n")).to_string();
        }
        if p.starts_with(":::") || p.starts_with("```") || p.starts_with("~~~") {
            continue;
        }
        if p.chars().count() > 500 {
            let cut = take_chars(&p, 500);
            let last_period = cut.rfind(". ").map(|i| cut[..i].chars().count());
            p = match last_period {
                Some(n) if n > 200 => take_chars(cut, n + 1).to_string(),
                _ => format!("{}…", rstrip(cut)),
            };
        }
        return p;
    }
    String::new()
}

fn docs_page_path(rel: &str, source_label: &str) -> String {
    let parts: Vec<&str> = rel.split('/').filter(|p| !p.is_empty()).collect();
    let source_dir = if source_label == "built-in" {
        "bundled"
    } else {
        "optional"
    };
    match parts.as_slice() {
        [category] => format!("{source_dir}/{category}/{category}-{category}"),
        [category, slug] => format!("{source_dir}/{category}/{category}-{slug}"),
        [category, sub, slug] => format!("{source_dir}/{category}/{category}-{sub}-{slug}"),
        _ => String::new(),
    }
}

/// The directories under `base` (itself included) that hold a `SKILL.md`
/// file, as `os.walk` finds them: symlinked directories are listed but not
/// entered. Sorted, so equal sort keys keep a stable order.
fn skill_dirs(base: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let mut entries: Vec<_> = fs::read_dir(base)
        .map_err(|error| format!("{}: {error}", base.display()))?
        .filter_map(Result::ok)
        .collect();
    entries.sort_by_key(|entry| entry.file_name());
    let mut has_skill = false;
    let mut subdirs = Vec::new();
    for entry in entries {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            subdirs.push(path);
        } else if entry.file_name() == "SKILL.md" && path.is_file() {
            has_skill = true;
        }
    }
    if has_skill {
        out.push(base.to_path_buf());
    }
    for dir in subdirs {
        skill_dirs(&dir, out)?;
    }
    Ok(())
}

pub(crate) fn rel_posix(path: &Path, base: &Path) -> String {
    let rel = path.strip_prefix(base).unwrap_or(path);
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    }
}

fn str_list(value: Option<&Py>) -> Vec<Py> {
    match value {
        Some(Py::List(items)) => items
            .iter()
            .filter(|x| truthy(x))
            .map(|x| Py::Str(py::str_of(x)))
            .collect(),
        Some(Py::Str(s)) if !py::strip(s).is_empty() => vec![Py::Str(py::strip(s).to_string())],
        _ => Vec::new(),
    }
}

fn extract_local_skills(root: &Path) -> Result<Vec<Py>, String> {
    let mut skills = Vec::new();
    for (base_dir, source_label) in [("skills", "built-in"), ("optional-skills", "optional")] {
        let base = root.join(base_dir);
        if !base.is_dir() {
            continue;
        }
        let mut dirs = Vec::new();
        skill_dirs(&base, &mut dirs)?;
        for dir in dirs {
            let content = read_text(&dir.join("SKILL.md"))?;
            let Some((fm_text, body)) = split_frontmatter(&content) else {
                continue;
            };
            let Some(fm) = load_yaml(fm_text) else {
                continue;
            };
            if !fm.is_dict() || !truthy(&fm) {
                continue;
            }
            let overview = extract_overview(py::strip(body));
            let rel = rel_posix(&dir, &base);
            let category = rel.split('/').next().unwrap_or_default().to_string();

            let mut tags = Py::List(Vec::new());
            if let Some(metadata @ Py::Dict(_)) = get(&fm, "metadata") {
                let hermes = get_or(metadata, "hermes", Py::Dict(Vec::new()));
                if hermes.is_dict() {
                    tags = get_or(&hermes, "tags", Py::List(Vec::new()));
                }
            }
            if !truthy(&tags) {
                tags = get_or(&fm, "tags", Py::List(Vec::new()));
            }
            if let Py::Str(s) = tags {
                tags = Py::List(vec![Py::Str(s)]);
            }
            if !truthy(&tags) {
                tags = Py::List(Vec::new());
            }

            let prereq = get(&fm, "prerequisites").filter(|p| truthy(p));
            let (env_vars, commands) = match prereq {
                Some(p @ Py::Dict(_)) => {
                    (str_list(get(p, "env_vars")), str_list(get(p, "commands")))
                }
                _ => (Vec::new(), Vec::new()),
            };
            let basename = dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();

            skills.push(Py::dict([
                ("name", get_or(&fm, "name", Py::Str(basename))),
                ("description", get_or(&fm, "description", "".into())),
                ("overview", overview.into()),
                ("category", category.clone().into()),
                ("categoryLabel", category_label(&category).into()),
                ("source", source_label.into()),
                ("tags", tags),
                ("platforms", get_or(&fm, "platforms", Py::List(Vec::new()))),
                ("author", get_or(&fm, "author", "".into())),
                ("version", get_or(&fm, "version", "".into())),
                ("license", get_or(&fm, "license", "".into())),
                ("envVars", Py::List(env_vars)),
                ("commands", Py::List(commands)),
                ("docsPath", docs_page_path(&rel, source_label).into()),
            ]));
        }
    }
    Ok(skills)
}

/// The tags Python's `for tag in tags` yields: a list's items, or a string's
/// characters.
fn tag_items(tags: &Py) -> Vec<String> {
    match tags {
        Py::List(items) => items.iter().map(|t| as_str(t).to_string()).collect(),
        Py::Str(s) => s.chars().map(String::from).collect(),
        _ => Vec::new(),
    }
}

fn guess_category(tags: &Py) -> String {
    let tags = tag_items(tags);
    let Some(first) = tags.first() else {
        return "uncategorized".to_string();
    };
    for tag in &tags {
        let lower = tag.to_lowercase();
        if let Some((category, _)) = TAG_CATEGORIES
            .iter()
            .find(|(_, t)| t.contains(&lower.as_str()))
        {
            return (*category).to_string();
        }
    }
    first.to_lowercase().replace(' ', "-")
}

fn list_of(value: &Py) -> &[Py] {
    match value {
        Py::List(items) => items,
        _ => &[],
    }
}

fn extract_cached_index_skills(root: &Path) -> Vec<Py> {
    let dir = root.join("skills/index-cache");
    let mut names: Vec<String> = match fs::read_dir(&dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".json"))
            .collect(),
        Err(_) => return Vec::new(),
    };
    names.sort();
    let mut skills = Vec::new();
    for filename in names {
        let Ok(text) = read_text(&dir.join(&filename)) else {
            continue;
        };
        let Ok(data) = py::loads(&text) else {
            continue;
        };
        let stem = filename.replace(".json", "");
        let source = SOURCE_LABELS
            .iter()
            .find(|(key, _)| stem.contains(key))
            .map_or("community", |(_, label)| label);

        if let Some(agents) = data.get("agents") {
            for agent in list_of(agents).iter().filter(|a| a.is_dict()) {
                let meta = get_or(agent, "meta", Py::Dict(Vec::new()));
                let name = get(agent, "identifier")
                    .cloned()
                    .unwrap_or_else(|| get_or(&meta, "title", "unknown".into()));
                let description = get(&meta, "description").filter(|d| truthy(d));
                let description = description.map(as_str).unwrap_or_default();
                let description =
                    take_chars(description.split('\n').next().unwrap_or_default(), 200);
                let tags = get_or(&meta, "tags", Py::List(Vec::new()));
                skills.push(Py::dict([
                    ("name", name),
                    ("description", description.into()),
                    ("category", guess_category(&tags).into()),
                    ("categoryLabel", "".into()),
                    ("source", source.into()),
                    ("tags", tags),
                    ("platforms", Py::List(Vec::new())),
                    ("author", get_or(agent, "author", "".into())),
                    ("version", "".into()),
                ]));
            }
            continue;
        }
        for entry in list_of(&data) {
            if !entry.is_dict() || !get(entry, "name").is_some_and(truthy) {
                continue;
            }
            if matches!(get(entry, "skills"), Some(Py::List(_))) {
                continue;
            }
            skills.push(Py::dict([
                ("name", get_or(entry, "name", "".into())),
                ("description", get_or(entry, "description", "".into())),
                ("category", "uncategorized".into()),
                ("categoryLabel", "".into()),
                ("source", source.into()),
                ("tags", get_or(entry, "tags", Py::List(Vec::new()))),
                ("platforms", Py::List(Vec::new())),
                ("author", "".into()),
                ("version", "".into()),
            ]));
        }
    }
    for skill in &mut skills {
        if as_str(get(skill, "categoryLabel").unwrap_or(&Py::Null)).is_empty() {
            let category = as_str(get(skill, "category").unwrap_or(&Py::Null)).to_string();
            let label = match lookup(&CATEGORY_LABELS, &category) {
                Some(label) => label.to_string(),
                None if category.is_empty() => "Uncategorized".to_string(),
                None => title(&category.replace('-', " ")),
            };
            skill.set("categoryLabel", label.into());
        }
    }
    skills
}

fn label_for_github_identifier(identifier: &str) -> &'static str {
    GITHUB_TAP_LABELS
        .iter()
        .find(|(prefix, _)| identifier == *prefix || identifier.starts_with(&format!("{prefix}/")))
        .map_or("GitHub", |(_, label)| label)
}

fn install_command(source: &str, identifier: &str, name: &str) -> String {
    let target = if identifier.is_empty() {
        name
    } else {
        identifier
    };
    if target.is_empty() {
        return "hermes skills install <skill>".to_string();
    }
    if source == "clawhub" && !target.starts_with("clawhub/") {
        return format!("hermes skills install clawhub/{target}");
    }
    format!("hermes skills install {target}")
}

/// `None` when the unified index is missing or unreadable, as the script's
/// `(None, None)`.
fn extract_unified_index_skills(root: &Path) -> Option<(Vec<Py>, Py)> {
    let path = root.join("website/static/api/skills-index.json");
    if !path.is_file() {
        return None;
    }
    let data = match read_text(&path).and_then(|text| py::loads(&text)) {
        Ok(data) => data,
        Err(error) => {
            println!("[extract-skills] Failed to read unified index: {error}");
            return None;
        }
    };
    let Some(Py::List(entries)) = data.get("skills") else {
        return None;
    };
    let meta = Py::dict([
        ("indexGeneratedAt", get_or(&data, "generated_at", "".into())),
        ("indexSkillCount", get_or(&data, "skill_count", Py::Int(0))),
        ("indexVersion", get_or(&data, "version", Py::Int(0))),
    ]);
    let mut out = Vec::new();
    for entry in entries.iter().filter(|e| e.is_dict()) {
        let or_empty = |key: &str| {
            get(entry, key)
                .filter(|v| truthy(v))
                .map(as_str)
                .unwrap_or_default()
        };
        let source_id = or_empty("source").to_lowercase();
        if source_id == "official" {
            continue;
        }
        let identifier = or_empty("identifier");
        let name = match get(entry, "name").filter(|v| truthy(v)) {
            Some(name) => name.clone(),
            None => match identifier.rsplit('/').next().unwrap_or_default() {
                "" => "unknown".into(),
                last => last.into(),
            },
        };
        let mut description = or_empty("description")
            .split('\n')
            .next()
            .unwrap_or_default()
            .to_string();
        if description.chars().count() > 280 {
            description = format!("{}...", take_chars(&description, 277));
        }
        let tags = match get(entry, "tags").filter(|v| truthy(v)) {
            Some(tags @ Py::List(_)) => tags.clone(),
            _ => Py::List(Vec::new()),
        };
        let source = if source_id == "github" {
            label_for_github_identifier(identifier).to_string()
        } else if let Some(label) = lookup(&UNIFIED_SOURCE_LABELS, &source_id) {
            label.to_string()
        } else if source_id.is_empty() {
            "community".to_string()
        } else {
            source_id.clone()
        };
        let repo = or_empty("repo");
        let author = if matches!(source_id.as_str(), "skills.sh" | "skills-sh") && !repo.is_empty()
        {
            repo.split('/').next().unwrap_or_default()
        } else {
            ""
        };
        let install = install_command(&source_id, identifier, as_str(&name));
        out.push(Py::dict([
            ("name", name),
            ("description", description.into()),
            ("overview", "".into()),
            ("category", guess_category(&tags).into()),
            ("categoryLabel", "".into()),
            ("source", source.into()),
            ("tags", tags),
            ("platforms", Py::List(Vec::new())),
            ("author", author.into()),
            ("version", "".into()),
            ("license", "".into()),
            ("envVars", Py::List(Vec::new())),
            ("commands", Py::List(Vec::new())),
            ("docsPath", "".into()),
            ("identifier", identifier.into()),
            ("installCmd", install.into()),
        ]));
    }
    Some((out, meta))
}

fn field<'a>(skill: &'a Py, key: &str) -> &'a str {
    as_str(get(skill, key).unwrap_or(&Py::Null))
}

fn consolidate_small_categories(skills: &mut [Py]) {
    let other = |skill: &mut Py| {
        skill.set("category", "other".into());
        skill.set("categoryLabel", "Other".into());
    };
    for skill in skills.iter_mut() {
        if matches!(field(skill, "category"), "uncategorized" | "") {
            other(skill);
        }
    }
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for skill in skills.iter() {
        *counts
            .entry(field(skill, "category").to_string())
            .or_default() += 1;
    }
    for skill in skills.iter_mut() {
        if counts[field(skill, "category")] < MIN_CATEGORY_SIZE {
            other(skill);
        }
    }
}

pub fn run_extract_skills(args: WebsiteArgs) -> Result<i32, String> {
    let root = resolve_argument(Path::new(&args.repo_root))?;
    let local = extract_local_skills(&root)?;
    let (external, index_meta, external_source) = match extract_unified_index_skills(&root) {
        Some((skills, meta)) => (skills, Some(meta), "unified-index"),
        None => {
            let skills = extract_cached_index_skills(&root);
            let source = if skills.is_empty() {
                "local-only"
            } else {
                "legacy-cache"
            };
            (skills, None, source)
        }
    };

    let mut all: Vec<Py> = local.iter().chain(&external).cloned().collect();
    consolidate_small_categories(&mut all);
    let source_order = |s: &str| match s {
        "built-in" => 0,
        "optional" => 1,
        _ => 2,
    };
    all.sort_by(|a, b| {
        let key = |s: &Py| {
            (
                source_order(field(s, "source")),
                field(s, "category") == "other",
                field(s, "category").to_string(),
                py::str_of(get(s, "name").unwrap_or(&Py::Null)),
            )
        };
        key(a).cmp(&key(b))
    });

    let data_dir = root.join("website/src/data");
    fs::create_dir_all(&data_dir).map_err(|error| format!("{}: {error}", data_dir.display()))?;
    let output = data_dir.join("skills.json");
    let meta_output = data_dir.join("skills-meta.json");
    write_text(&output, &py::dumps(&Py::List(all.clone()), Some(2), false))?;

    let mut by_source: BTreeMap<String, usize> = BTreeMap::new();
    for skill in &all {
        *by_source
            .entry(field(skill, "source").to_string())
            .or_default() += 1;
    }
    let generated_at = index_meta
        .as_ref()
        .and_then(|m| m.get("indexGeneratedAt").cloned())
        .unwrap_or_else(|| "".into());
    let (status, detail) = match external_source {
        "local-only" => (
            "degraded",
            "unified skills index unavailable; only local skills were extracted",
        ),
        "legacy-cache" => ("degraded", "using legacy skills/index-cache fallback"),
        _ => ("ok", "skills metadata extracted"),
    };
    let mut meta = Py::dict([
        ("extractedAt", py::utc_isoformat().into()),
        ("indexGeneratedAt", generated_at),
        ("totalSkills", all.len().into()),
        ("externalSource", external_source.into()),
        (
            "indexHealth",
            Py::dict([("status", status.into()), ("detail", detail.into())]),
        ),
        (
            "bySource",
            Py::dict(by_source.into_iter().map(|(k, v)| (k, v.into()))),
        ),
    ]);
    if let Some(Py::Dict(pairs)) = index_meta {
        for (key, value) in pairs {
            meta.set(&key, value);
        }
    }
    write_text(&meta_output, &py::dumps(&meta, Some(2), false))?;

    let count = |label: &str| local.iter().filter(|s| field(s, "source") == label).count();
    println!(
        "Extracted {} skills to {}",
        all.len(),
        py::path_str(&output)
    );
    println!(
        "Wrote metadata to {} ({external_source})",
        py::path_str(&meta_output)
    );
    println!(
        "  {} local ({} built-in, {} optional)",
        local.len(),
        count("built-in"),
        count("optional")
    );
    println!("  {} from external indexes", external.len());
    Ok(0)
}

#[cfg(test)]
#[path = "website_tests.rs"]
mod tests;
