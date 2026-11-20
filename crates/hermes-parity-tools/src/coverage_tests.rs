//! Index syntactically declared Rust tests and follow split test modules.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};

use syn::ext::IdentExt;
use syn::{Attribute, Expr, Item, Lit, LitStr, Meta};

pub(crate) type TestCatalogue = BTreeMap<PathBuf, BTreeSet<String>>;

enum Member {
    Test(String),
    Include(PathBuf),
    Module {
        name: String,
        path: Option<PathBuf>,
        inline: Option<Vec<Member>>,
    },
}

fn members(items: Vec<Item>) -> Vec<Member> {
    items
        .into_iter()
        .filter_map(|item| match item {
            Item::Fn(function) if function.attrs.iter().any(is_test_attribute) => {
                Some(Member::Test(function.sig.ident.unraw().to_string()))
            }
            Item::Mod(module) => Some(Member::Module {
                name: module.ident.unraw().to_string(),
                path: module.attrs.iter().find_map(module_path),
                inline: module.content.map(|(_, items)| members(items)),
            }),
            Item::Macro(item) if item.mac.path.is_ident("include") => {
                syn::parse2::<LitStr>(item.mac.tokens)
                    .ok()
                    .map(|literal| Member::Include(literal.value().into()))
            }
            _ => None,
        })
        .collect()
}

fn is_test_attribute(attribute: &Attribute) -> bool {
    let path = attribute.path();
    path.is_ident("test")
        || (path.segments.len() == 2
            && path.segments[0].ident == "tokio"
            && path.segments[1].ident == "test")
}

fn module_path(attribute: &Attribute) -> Option<PathBuf> {
    if !attribute.path().is_ident("path") {
        return None;
    }
    let Meta::NameValue(value) = &attribute.meta else {
        return None;
    };
    let Expr::Lit(literal) = &value.value else {
        return None;
    };
    match &literal.lit {
        Lit::Str(path) => Some(path.value().into()),
        _ => None,
    }
}

fn collect_sources(
    root: &Path,
    directory: &Path,
    sources: &mut BTreeMap<PathBuf, Vec<Member>>,
) -> Result<(), String> {
    let entries = match fs::read_dir(root.join(directory)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("could not list {}: {error}", directory.display())),
    };
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let relative = directory.join(entry.file_name());
        let kind = entry.file_type().map_err(|error| error.to_string())?;
        if kind.is_dir() {
            collect_sources(root, &relative, sources)?;
        } else if rust_source(&relative) && entry.path().is_file() {
            let bytes = fs::read(entry.path())
                .map_err(|error| format!("could not read {}: {error}", relative.display()))?;
            let source = String::from_utf8_lossy(&bytes);
            // Fixtures and include fragments can be non-item Rust. They
            // cannot supply a standalone test declaration.
            if let Ok(tree) = syn::parse_file(&source) {
                sources.insert(relative, members(tree.items));
            }
        }
    }
    Ok(())
}

fn rust_source(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| {
            if cfg!(windows) {
                extension.eq_ignore_ascii_case("rs")
            } else {
                extension == "rs"
            }
        })
}

fn normalise(path: &Path) -> Option<PathBuf> {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => result.push(part),
            Component::CurDir => {}
            Component::ParentDir if result.pop() => {}
            _ => return None,
        }
    }
    Some(result)
}

fn module_directory(file: &Path) -> PathBuf {
    let is_target = file.components().count() == 2 && file.starts_with("tests")
        || file.ancestors().nth(2).is_some_and(|ancestor| {
            ancestor.parent() == Some(Path::new("crates"))
                && file.parent().and_then(Path::file_name).is_some_and(|area| {
                    matches!(area.to_str(), Some("tests" | "examples" | "benches"))
                })
        })
        || file
            .parent()
            .is_some_and(|parent| parent.ends_with("src/bin"));
    if is_target
        || matches!(
            file.file_name().and_then(OsStr::to_str),
            Some("mod.rs" | "lib.rs" | "main.rs")
        )
    {
        file.parent().unwrap_or(Path::new("")).to_path_buf()
    } else {
        file.with_extension("")
    }
}

struct Search<'a> {
    sources: &'a BTreeMap<PathBuf, Vec<Member>>,
    ancestors: HashSet<PathBuf>,
    names: BTreeSet<String>,
    followed_modules: bool,
}

impl Search<'_> {
    fn visit_file(&mut self, file: &Path, module_dir: &Path) {
        let Some(items) = self.sources.get(file) else {
            return;
        };
        if self.ancestors.insert(file.to_owned()) {
            self.visit_members(items, file, module_dir, None);
            self.ancestors.remove(file);
        }
    }

    fn visit_members(
        &mut self,
        items: &[Member],
        file: &Path,
        module_dir: &Path,
        inline_dir: Option<&Path>,
    ) {
        let physical_dir = file.parent().unwrap_or(Path::new(""));
        let search_dir = inline_dir.unwrap_or(module_dir);
        let attribute_dir = inline_dir.unwrap_or(physical_dir);
        for member in items {
            match member {
                Member::Test(name) => {
                    self.names.insert(name.clone());
                }
                Member::Include(path) => {
                    if let Some(child) = normalise(&physical_dir.join(path)) {
                        self.followed_modules = true;
                        self.visit_file(&child, search_dir);
                    }
                }
                Member::Module {
                    name,
                    path,
                    inline: Some(items),
                } => {
                    let directory = match path {
                        Some(path) => attribute_dir.join(path),
                        None => search_dir.join(name),
                    };
                    self.visit_members(items, file, module_dir, Some(&directory));
                }
                Member::Module {
                    path: Some(path),
                    inline: None,
                    ..
                } => {
                    if let Some(child) = normalise(&attribute_dir.join(path)) {
                        self.followed_modules = true;
                        self.visit_file(&child, child.parent().unwrap_or(Path::new("")));
                    }
                }
                Member::Module {
                    name,
                    path: None,
                    inline: None,
                } => {
                    self.followed_modules = true;
                    for child in [
                        search_dir.join(format!("{name}.rs")),
                        search_dir.join(name).join("mod.rs"),
                    ] {
                        if let Some(child) = normalise(&child) {
                            self.visit_file(&child, &module_directory(&child));
                        }
                    }
                }
            }
        }
    }
}

pub(crate) fn scan(root: &Path) -> Result<TestCatalogue, String> {
    let mut sources = BTreeMap::new();
    for dir in ["crates", "tests"] {
        collect_sources(root, Path::new(dir), &mut sources)?;
    }
    let mut tests = TestCatalogue::new();
    for file in sources.keys() {
        let mut search = Search {
            sources: &sources,
            ancestors: HashSet::new(),
            names: BTreeSet::new(),
            followed_modules: false,
        };
        search.visit_file(file, &module_directory(file));
        if !search.names.is_empty() {
            if search.followed_modules && file.file_name() == Some(OsStr::new("mod.rs")) {
                // Existing manifests also cite the historical sibling facade.
                if let Some(parent) = file.parent() {
                    let mut alias = parent.as_os_str().to_owned();
                    alias.push(".rs");
                    tests
                        .entry(PathBuf::from(alias))
                        .or_default()
                        .extend(search.names.iter().cloned());
                }
            }
            tests.entry(file.clone()).or_default().extend(search.names);
        }
    }
    Ok(tests)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, file: &str, source: &str) {
        let path = root.join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, source).unwrap();
    }

    #[test]
    fn rust_extension_uses_platform_glob_case_rules() {
        assert!(rust_source(Path::new("module.rs")));
        assert_eq!(rust_source(Path::new("module.RS")), cfg!(windows));
        assert_eq!(rust_source(Path::new("module.Rs")), cfg!(windows));
        assert!(!rust_source(Path::new("module.txt")));
    }

    #[test]
    fn follows_restricted_attributed_and_nested_rust_modules() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "crates/demo/src/lib.rs", "mod suite;");
        write(
            root,
            "crates/demo/src/suite.rs",
            r##"
            #[test] fn direct() {}
            fn helper() {}
            mod inner {
                pub(super) mod narrow;
                pub(in crate::suite) mod scoped;
                #[allow(dead_code)] mod same_line;
                #[path = "fixtures/test_list.rs"] mod renamed;
            }
            include!("literal_include.rs");
            const EXAMPLE: &str = "#[test] fn text() {}";
        "##,
        );
        for (file, name) in [
            ("suite/inner/narrow.rs", "restricted_super"),
            ("suite/inner/scoped/mod.rs", "restricted_path"),
            ("suite/inner/same_line.rs", "same_line"),
            ("suite/inner/fixtures/test_list.rs", "renamed_test"),
            ("literal_include.rs", "included_test"),
        ] {
            write(
                root,
                &format!("crates/demo/src/{file}"),
                &format!("#[test] fn {name}() {{}}"),
            );
        }
        let tests = scan(root).unwrap();
        let expected = [
            "direct",
            "included_test",
            "renamed_test",
            "restricted_path",
            "restricted_super",
            "same_line",
        ]
        .map(str::to_owned)
        .into_iter()
        .collect();
        assert_eq!(tests[Path::new("crates/demo/src/suite.rs")], expected);
        assert_eq!(tests[Path::new("crates/demo/src/lib.rs")], expected);
    }

    #[test]
    fn counts_only_literal_items_and_stops_include_cycles() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "tests/root.rs",
            r#"
            include!("loop.rs");
            mod nested {
                #[tokio::test(flavor = "current_thread")] async fn real_test() {}
                fn helper() {}
            }
            // #[test] fn commented() {}
            macro_rules! defines_test { () => { #[test] fn generated() {} } }
        "#,
        );
        write(root, "tests/loop.rs", "include!(\"root.rs\");");
        let tests = scan(root).unwrap();
        assert_eq!(
            tests[Path::new("tests/root.rs")],
            BTreeSet::from(["real_test".to_owned()])
        );
    }
}
