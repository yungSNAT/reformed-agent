use super::*;
use tempfile::tempdir;
use wiremock::matchers::{method, path as url_path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn mk_ref(kind: &str, target: &str) -> ContextReference {
    ContextReference {
        raw: format!("@{kind}:{target}"),
        kind: kind.to_string(),
        target: target.to_string(),
        start: 0,
        end: 0,
        line_start: None,
        line_end: None,
    }
}

#[test]
fn parse_empty_message_returns_no_references() {
    assert!(parse_context_references("").is_empty());
}

#[tokio::test]
async fn hard_limit_blocks_injection() {
    let td = tempdir().unwrap();
    std::fs::write(td.path().join("big.txt"), "word ".repeat(2000)).unwrap();
    let msg = "read @file:big.txt";
    let result = preprocess_context_references_async(msg, td.path(), 100, None).await;
    assert!(result.blocked);
    assert!(!result.expanded);
    assert_eq!(result.message, msg);
    assert_eq!(result.original_message, msg);
    assert!(result.injected_tokens > 50);
    assert!(result
        .warnings
        .iter()
        .any(|w| w.contains("50% hard limit (50)")));
}

#[tokio::test]
async fn soft_limit_warns_but_expands() {
    let td = tempdir().unwrap();
    std::fs::write(td.path().join("small.txt"), "hello world\n".repeat(20)).unwrap();
    let msg = "read @file:small.txt";
    let first = preprocess_context_references_async(msg, td.path(), 10_000_000, None).await;
    let tokens = first.injected_tokens;
    assert!(tokens > 0);
    assert!(first.warnings.is_empty());

    let result = preprocess_context_references_async(msg, td.path(), tokens * 2, None).await;
    assert!(!result.blocked);
    assert!(result.expanded);
    assert!(result.warnings.iter().any(|w| w.contains("25% soft limit")));
    assert!(result.message.contains("--- Context Warnings ---"));
    assert!(result.message.contains("--- Attached Context ---"));
    assert!(result.message.contains("hello world"));
}

#[tokio::test]
async fn file_reference_error_paths() {
    let td = tempdir().unwrap();
    std::fs::create_dir_all(td.path().join("sub")).unwrap();
    std::fs::write(td.path().join("bin.dat"), [1u8, 2, 0, 3]).unwrap();
    std::fs::write(td.path().join("bad.txt"), [0xffu8, 0xfe, 0x41, 0x42]).unwrap();

    let msg = "check @file:missing.txt @file:sub @file:bin.dat @file:bad.txt";
    let result = preprocess_context_references_async(msg, td.path(), 100_000, None).await;
    assert!(result.expanded);
    assert_eq!(result.injected_tokens, 0);
    assert_eq!(result.warnings.len(), 4);
    assert!(result
        .warnings
        .contains(&"@file:missing.txt: file not found".to_string()));
    assert!(result
        .warnings
        .contains(&"@file:sub: path is not a file".to_string()));
    assert!(result
        .warnings
        .contains(&"@file:bin.dat: binary files are not supported".to_string()));
    assert!(result
        .warnings
        .iter()
        .any(|w| w.starts_with("@file:bad.txt: failed to read file (")));
    assert!(!result.message.contains("Attached Context"));
}

#[tokio::test]
async fn folder_reference_success_and_errors() {
    let td = tempdir().unwrap();
    let sub = td.path().join("sub");
    std::fs::create_dir_all(sub.join("deep")).unwrap();
    std::fs::write(sub.join("a.txt"), "abc").unwrap();
    std::fs::write(sub.join("deep").join("b.rs"), "x").unwrap();

    let result =
        preprocess_context_references_async("list @folder:sub please", td.path(), 100_000, None)
            .await;
    assert!(result.expanded);
    assert!(result.warnings.is_empty(), "{:?}", result.warnings);
    assert!(result.message.contains("📁 @folder:sub ("));
    assert!(result.message.contains("sub/\n- a.txt (3 bytes)"));
    assert!(result.message.contains("- deep/"));
    assert!(result.message.contains("  - b.rs (1 bytes)"));

    let result = preprocess_context_references_async(
        "x @folder:missing_dir @folder:sub/a.txt @folder:../",
        td.path(),
        100_000,
        None,
    )
    .await;
    assert_eq!(result.injected_tokens, 0);
    assert!(result
        .warnings
        .contains(&"@folder:missing_dir: folder not found".to_string()));
    assert!(result
        .warnings
        .contains(&"@folder:sub/a.txt: path is not a folder".to_string()));
    assert!(result
        .warnings
        .iter()
        .any(|w| w.starts_with("@folder:../:") && w.contains("outside the allowed workspace")));
}

#[tokio::test]
async fn folder_reference_blocks_sensitive_home_dir() {
    let Some(home) = home_dir() else {
        return;
    };
    let home_c = canonicalize_or_normalize(&home);
    let reference = mk_ref("folder", ".ssh/zz_coverage_missing_dir");
    let (warning, block) = expand_folder_reference(&reference, &home_c, &home_c).await;
    if home_dir().as_deref() != Some(home.as_path()) {
        return;
    }
    assert!(block.is_none());
    let warning = warning.expect("warning expected");
    assert!(
        warning.contains("sensitive credential or internal Hermes path"),
        "{warning}"
    );
}

#[tokio::test]
async fn unsupported_reference_kind_warns() {
    let reference = mk_ref("bogus", "x");
    let (warning, block) = expand_reference(&reference, Path::new("."), Path::new(".")).await;
    assert!(block.is_none());
    assert_eq!(
        warning.as_deref(),
        Some("@bogus:x: unsupported reference type 'bogus'")
    );
}

#[tokio::test]
async fn git_reference_outside_repository_warns() {
    let td = tempdir().unwrap();
    let result = preprocess_context_references_async("@staged", td.path(), 100_000, None).await;
    assert_eq!(result.injected_tokens, 0);
    assert_eq!(result.warnings.len(), 1, "{:?}", result.warnings);
    let w = &result.warnings[0];
    assert!(w.starts_with("@staged: "), "{w}");
    assert!(
        w.contains("git diff failed")
            || w.contains("could not be run safely")
            || w.contains("timed out"),
        "{w}"
    );
    assert!(!result.message.contains("Attached Context"));
}

#[tokio::test]
async fn diff_in_clean_repository_reports_no_output() {
    let td = tempdir().unwrap();
    let repo = td.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let Ok(out) = std::process::Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["init", "-q"])
        .output()
    else {
        return;
    };
    if !out.status.success() || read_only_git(&repo).is_none() {
        return;
    }
    let result = preprocess_context_references_async("@diff", &repo, 100_000, None).await;
    assert!(result.warnings.is_empty(), "{:?}", result.warnings);
    assert!(
        result.message.contains("🧾 git diff ("),
        "{}",
        result.message
    );
    assert!(result.message.contains("(no output)"), "{}", result.message);
}

#[tokio::test]
async fn url_reference_empty_and_invalid() {
    let (warning, block) = expand_url_reference(&mk_ref("url", "   ")).await;
    assert!(block.is_none());
    assert_eq!(warning.as_deref(), Some("@url:   : empty URL"));

    let (warning, block) = expand_url_reference(&mk_ref("url", "not a valid url")).await;
    assert!(block.is_none());
    assert!(warning.unwrap().contains("failed to fetch URL ("));
}

#[tokio::test]
async fn url_reference_fetches_from_mock_server() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(url_path("/ok"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "<html><head><style>p{color:red}</style><script>var x=1;</script></head>\
             <body><p>Hello   <b>World</b></p></body></html>",
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(url_path("/empty"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<div>   </div>"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(url_path("/missing"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let ok = mk_ref("url", &format!("{}/ok", server.uri()));
    let (warning, block) = expand_reference(&ok, Path::new("."), Path::new(".")).await;
    assert!(warning.is_none(), "{warning:?}");
    let block = block.unwrap();
    assert!(block.starts_with("🌐 @url:"));
    assert!(block.contains("Hello World"));
    assert!(!block.contains("var x"));
    assert!(!block.contains("color:red"));

    let empty = mk_ref("url", &format!("{}/empty", server.uri()));
    let (warning, block) = expand_reference(&empty, Path::new("."), Path::new(".")).await;
    assert!(block.is_none());
    assert!(warning.unwrap().ends_with(": no content extracted"));

    let missing = mk_ref("url", &format!("{}/missing", server.uri()));
    let (warning, block) = expand_reference(&missing, Path::new("."), Path::new(".")).await;
    assert!(block.is_none());
    assert!(warning.unwrap().contains("failed to fetch URL (HTTP 404"));
}

#[test]
fn sanitize_html_strips_and_truncates() {
    assert_eq!(
        sanitize_html_to_text("<script>a()</script><style>b{}</style><p>Hi \n\n <i>there</i></p>"),
        "Hi there"
    );
    let long = "a".repeat(MAX_URL_CHARS + 500);
    assert_eq!(sanitize_html_to_text(&long).len(), MAX_URL_CHARS);
    assert_eq!(sanitize_html_to_text("<br/>"), "");
}

#[test]
fn remove_reference_tokens_without_refs_trims() {
    assert_eq!(
        remove_reference_tokens("  hello there  ", &[]),
        "hello there"
    );
}

#[test]
fn resolve_path_without_allowed_root() {
    let td = tempdir().unwrap();
    let resolved = resolve_path(td.path(), "a/../zz_missing_cov.txt", None).unwrap();
    assert_eq!(resolved, td.path().join("zz_missing_cov.txt"));
}

#[test]
fn expand_user_path_handles_home_forms() {
    let before = home_dir();
    let tilde = expand_user_path("~");
    let slash = expand_user_path("~/");
    let nested = expand_user_path("~/docs/notes.txt");
    if home_dir() != before {
        return;
    }
    match before {
        Some(home) => {
            assert_eq!(tilde, home);
            assert_eq!(slash, home);
            assert_eq!(nested, home.join("docs/notes.txt"));
        }
        None => {
            assert_eq!(tilde, PathBuf::from("~"));
            assert_eq!(slash, PathBuf::from("~/"));
        }
    }
}

#[test]
fn expand_user_path_rejects_invalid_and_unknown_users() {
    assert_eq!(
        expand_user_path("~bad user!/x"),
        PathBuf::from("~bad user!/x")
    );
    assert_eq!(
        expand_user_path("~zzcov_no_such_user_918/x"),
        PathBuf::from("~zzcov_no_such_user_918/x")
    );
    assert_eq!(
        expand_user_path("~zzcov_no_such_user_918"),
        PathBuf::from("~zzcov_no_such_user_918")
    );
    assert_eq!(expand_user_path("plain/path"), PathBuf::from("plain/path"));
}

#[test]
fn expand_user_path_for_current_user() {
    let Some(user) = current_username() else {
        return;
    };
    assert!(!user.trim().is_empty());
    if !is_valid_username(&user) {
        return;
    }
    let before = home_dir();
    let got = expand_user_path(&format!("~{user}/sub"));
    let got_bare = expand_user_path(&format!("~{user}"));
    if home_dir() != before || current_username().as_deref() != Some(user.as_str()) {
        return;
    }
    if let Some(home) = before {
        assert_eq!(got, home.join("sub"));
        assert_eq!(got_bare, home);
    }
}

#[cfg(unix)]
#[test]
fn expand_user_path_reads_passwd_entry() {
    let Ok(passwd) = std::fs::read_to_string("/etc/passwd") else {
        return;
    };
    let Some(line) = passwd
        .lines()
        .find(|l| !l.is_empty() && !l.starts_with('#'))
    else {
        return;
    };
    let fields: Vec<&str> = line.split(':').collect();
    if fields.len() < 6 {
        return;
    }
    let user = fields[0];
    if !is_valid_username(user) || current_username().as_deref() == Some(user) {
        return;
    }
    assert_eq!(
        expand_user_path(&format!("~{user}/abc")),
        PathBuf::from(fields[5]).join("abc")
    );
    assert_eq!(
        lookup_home_for_username(user),
        Some(PathBuf::from(fields[5]))
    );
}

#[test]
fn username_validation() {
    assert!(is_valid_username("alice.b_c-1"));
    assert!(!is_valid_username(""));
    assert!(!is_valid_username("bad user"));
    assert!(!is_valid_username("x/y"));
}

#[test]
fn lexical_normalization() {
    assert_eq!(
        normalize_path_lexical(Path::new("a/./b/../c")),
        PathBuf::from("a/c")
    );
    assert_eq!(normalize_path_lexical(Path::new("")), PathBuf::from("."));
    assert_eq!(normalize_path_lexical(Path::new("./.")), PathBuf::from("."));
    assert_eq!(
        normalize_path_lexical(Path::new("a/..")),
        PathBuf::from(".")
    );
    assert_eq!(
        normalize_path_lexical(Path::new("/x/../..")),
        PathBuf::from("/")
    );
    assert_eq!(
        canonicalize_or_normalize(Path::new("/zz_cov_definitely/not/../here_cov")),
        PathBuf::from("/zz_cov_definitely/here_cov")
    );
}

#[test]
fn strip_trailing_brackets() {
    assert_eq!(strip_trailing_punctuation("foo]".to_string()), "foo");
    assert_eq!(strip_trailing_punctuation("x}".to_string()), "x");
    assert_eq!(strip_trailing_punctuation("a[b]".to_string()), "a[b]");
    assert_eq!(strip_trailing_punctuation("a{b}".to_string()), "a{b}");
    assert_eq!(strip_trailing_punctuation("f(x)).".to_string()), "f(x)");
}

#[test]
fn select_line_range_empty_text() {
    assert_eq!(select_line_range("", 1, 3), "");
}

#[test]
fn binary_check_on_missing_file_is_false() {
    let td = tempdir().unwrap();
    assert!(!is_binary_file(&td.path().join("nope.bin")));
}

#[test]
fn code_fence_languages() {
    let cases = [
        ("a.md", "markdown"),
        ("a.sh", "bash"),
        ("a.go", "go"),
        ("A.java", "java"),
        ("a.kt", "kotlin"),
        ("a.c", "c"),
        ("a.h", "c"),
        ("a.cc", "cpp"),
        ("a.cpp", "cpp"),
        ("a.hpp", "cpp"),
        ("a.unknownext", ""),
        ("Makefile", ""),
    ];
    for (name, lang) in cases {
        assert_eq!(code_fence_language(Path::new(name)), lang, "{name}");
    }
}

#[test]
fn folder_listing_structure_and_limits() {
    let td = tempdir().unwrap();
    let root = td.path();
    std::fs::write(root.join("a.txt"), "abc").unwrap();
    std::fs::write(root.join(".hidden"), "h").unwrap();
    std::fs::create_dir_all(root.join("node_modules")).unwrap();
    std::fs::write(root.join("node_modules").join("m.js"), "m").unwrap();
    let nested = root.join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(nested.join("inner.rs"), "x").unwrap();

    assert_eq!(
        build_folder_listing(root, root, 200),
        "./\n- a.txt (3 bytes)\n- nested/\n  - inner.rs (1 bytes)"
    );
    assert_eq!(
        build_folder_listing(root, root, 1),
        "./\n- a.txt (3 bytes)\n- ..."
    );
    assert_eq!(
        build_folder_listing(&nested, Path::new("/zz_cov_not_a_prefix"), 200),
        format!("{}/\n- inner.rs (1 bytes)", nested.display())
    );
    assert_eq!(
        build_folder_listing(&root.join("missing"), root, 10),
        "missing/"
    );
}
