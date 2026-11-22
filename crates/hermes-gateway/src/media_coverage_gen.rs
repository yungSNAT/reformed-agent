use super::*;
use std::process::Command;

const CHILD_FIXTURE_ENV: &str = "HERMES_GATEWAY_MEDIA_COVERAGE_CHILD_FIXTURE";
const CHILD_OK: &str = "media coverage fixture assertions passed";

fn child_test(name: &str, fixture: &Path) -> Command {
    // libtest names omit the crate-name component of module_path!().
    let (_, module) = module_path!().split_once("::").unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "--nocapture"])
        .arg(format!("{module}::{name}"))
        .env(CHILD_FIXTURE_ENV, fixture);
    command
}

fn assert_child_passed(command: &mut Command) {
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains(CHILD_OK),
        "child exited with {}\nstdout:\n{stdout}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn cache_config(path: &Path) -> MediaCacheConfig {
    MediaCacheConfig {
        cache_dir: path.to_string_lossy().into_owned(),
        max_size: 0,
        ttl_seconds: 0,
        max_file_size: 0,
    }
}

fn delivery_policy(fixture: &Path) -> MediaDeliveryPolicy {
    MediaDeliveryPolicy {
        home: None,
        hermes_home: fixture.join("hermes"),
        hermes_root: None,
        allowed_roots: Vec::new(),
        denied_prefixes: Vec::new(),
        strict_mode: true,
        recent_window: Duration::ZERO,
    }
}

fn write_at(path: &Path, data: &[u8], modified: SystemTime) {
    std::fs::write(path, data).unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(modified)
        .unwrap();
}

#[test]
fn environment_overrides_are_parsed_in_an_isolated_process() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(
            module_path!(),
            "::environment_overrides_are_parsed_in_an_isolated_process"
        );
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    const UNKNOWN_BOOLEAN_ENV: &str = "HERMES_MEDIA_COVERAGE_UNKNOWN_BOOLEAN";

    if let Some(fixture) = std::env::var_os(CHILD_FIXTURE_ENV) {
        let fixture = PathBuf::from(fixture);
        let home = fixture.join("home");
        let cache_dir = fixture.join("default-cache");

        assert_eq!(
            extra_media_allow_roots_from_env(),
            vec![home.join("shared"), home.clone(), home.join("inbox")]
        );
        assert!(truthy_env(MEDIA_DELIVERY_STRICT_ENV, false));
        assert!(!truthy_env(MEDIA_DELIVERY_TRUST_RECENT_ENV, true));
        assert!(truthy_env(UNKNOWN_BOOLEAN_ENV, true));
        assert!(!truthy_env(UNKNOWN_BOOLEAN_ENV, false));
        assert_eq!(media_delivery_recency_window_from_env(), Duration::ZERO);

        assert!(!cache_dir.exists());
        let cache = MediaCache::with_defaults().unwrap();
        assert_eq!(cache.cache_dir(), cache_dir);
        assert!(cache_dir.is_dir());
        assert_eq!(cache.max_file_size, DEFAULT_INBOUND_MEDIA_MAX_BYTES);
        println!("{CHILD_OK}");
        return;
    }

    let fixture = tempfile::tempdir().unwrap();
    let home = fixture.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let separator = if cfg!(windows) { ';' } else { ':' };
    assert_child_passed(
        child_test(
            "environment_overrides_are_parsed_in_an_isolated_process",
            fixture.path(),
        )
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env(
            "HERMES_MEDIA_CACHE_DIR",
            fixture.path().join("default-cache"),
        )
        .env(
            MEDIA_DELIVERY_ALLOW_DIRS_ENV,
            format!("  ~/shared, ,relative {separator} ~ {separator} ~/inbox  "),
        )
        .env(MEDIA_DELIVERY_STRICT_ENV, " YeS ")
        .env(MEDIA_DELIVERY_TRUST_RECENT_ENV, " oFf ")
        .env(MEDIA_DELIVERY_TRUST_RECENT_SECONDS_ENV, "42.5")
        .env(UNKNOWN_BOOLEAN_ENV, "sometimes"),
    );
}

#[test]
fn path_helpers_expand_home_and_reject_empty_or_nul_input() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(
            module_path!(),
            "::path_helpers_expand_home_and_reject_empty_or_nul_input"
        );
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let fixture = tempfile::tempdir().unwrap();
    let home = fixture.path();
    assert_eq!(expand_home_path("~", Some(home)), Some(home.to_path_buf()));
    assert_eq!(
        expand_home_path("~/report.pdf", Some(home)),
        Some(home.join("report.pdf"))
    );
    assert_eq!(expand_home_path("~/report.pdf", None), None);
    assert_eq!(expand_home_path("bad\0path", Some(home)), None);

    for raw in [" \t ", "bad\0path", " ` ` ", "');]"] {
        assert_eq!(strip_model_path_wrapping(raw), None, "input: {raw:?}");
    }
    assert_eq!(
        strip_model_path_wrapping("  \"report.pdf;\"  ").as_deref(),
        Some("report.pdf")
    );

    let missing = PathBuf::from(format!("missing-media-{}", uuid::Uuid::new_v4()));
    assert_eq!(
        canonicalize_or_absolute(&missing),
        Some(std::env::current_dir().unwrap().join(missing))
    );
}

#[cfg(target_os = "linux")]
#[test]
fn delivery_skips_unresolvable_relative_roots_when_cwd_is_gone() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(
            module_path!(),
            "::delivery_skips_unresolvable_relative_roots_when_cwd_is_gone"
        );
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    if let Some(fixture) = std::env::var_os(CHILD_FIXTURE_ENV) {
        let fixture = PathBuf::from(fixture);
        let report = fixture.join("report.txt");
        let mut policy = delivery_policy(&fixture);
        policy.strict_mode = false;
        policy.allowed_roots.push(PathBuf::from("relative-allow"));
        policy.denied_prefixes.push(PathBuf::from("relative-deny"));

        // Only this child uses the empty working directory.
        std::fs::remove_dir(fixture.join("working")).unwrap();
        let cwd_was_missing = std::env::current_dir().is_err();
        let validated =
            validate_media_delivery_path_with_policy(&report.to_string_lossy(), &policy);
        std::env::set_current_dir(&fixture).unwrap();

        assert!(cwd_was_missing);
        assert_eq!(validated, Some(std::fs::canonicalize(&report).unwrap()));
        println!("{CHILD_OK}");
        return;
    }

    let fixture = tempfile::tempdir().unwrap();
    let working = fixture.path().join("working");
    std::fs::create_dir(&working).unwrap();
    std::fs::write(fixture.path().join("report.txt"), b"report").unwrap();
    assert_child_passed(
        child_test(
            "delivery_skips_unresolvable_relative_roots_when_cwd_is_gone",
            fixture.path(),
        )
        .current_dir(working),
    );
}

#[test]
fn delivery_checks_file_type_recency_allow_roots_and_credentials() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(
            module_path!(),
            "::delivery_checks_file_type_recency_allow_roots_and_credentials"
        );
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let fixture = tempfile::tempdir().unwrap();
    let home = fixture.path().join("home");
    let exports = home.join("exports");
    let credentials = home.join(".ssh");
    std::fs::create_dir_all(&exports).unwrap();
    std::fs::create_dir_all(&credentials).unwrap();

    let recent = home.join("recent.txt");
    let old = exports.join("old.txt");
    let secret = credentials.join("test-key");
    let now = SystemTime::now();
    let recent_time = now - Duration::from_secs(60);
    write_at(&recent, b"recent", recent_time);
    write_at(&old, b"old", now - Duration::from_secs(7200));
    write_at(&secret, b"fixture", recent_time);

    let mut policy = delivery_policy(fixture.path());
    policy.home = Some(home.clone());
    let validate = |path: &Path, policy: &MediaDeliveryPolicy| {
        validate_media_delivery_path_with_policy(&path.to_string_lossy(), policy)
    };
    assert_eq!(validate(Path::new("recent.txt"), &policy), None);
    assert_eq!(validate(&exports, &policy), None);
    assert_eq!(validate(&recent, &policy), None);

    policy.recent_window = Duration::from_secs(3600);
    assert!(!file_is_recently_produced(
        &home.join("missing.txt"),
        policy.recent_window
    ));
    assert_eq!(
        validate(&recent, &policy),
        Some(std::fs::canonicalize(&recent).unwrap())
    );
    assert_eq!(validate(&old, &policy), None);
    assert_eq!(validate(&secret, &policy), None);

    write_at(&recent, b"future", now + Duration::from_secs(7200));
    assert_eq!(validate(&recent, &policy), None);

    let canonical_old = std::fs::canonicalize(&old).unwrap();
    policy.strict_mode = false;
    assert_eq!(validate(&old, &policy), Some(canonical_old.clone()));
    assert_eq!(validate(&secret, &policy), None);

    policy.strict_mode = true;
    policy.allowed_roots.push(exports);
    assert_eq!(validate(&old, &policy), Some(canonical_old));
}

#[test]
fn new_cache_reports_a_file_in_place_of_the_cache_directory() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(
            module_path!(),
            "::new_cache_reports_a_file_in_place_of_the_cache_directory"
        );
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let fixture = tempfile::tempdir().unwrap();
    let cache_path = fixture.path().join("not-a-directory");
    std::fs::write(&cache_path, b"keep me").unwrap();

    let Err(GatewayError::ConnectionFailed(message)) = MediaCache::new(&cache_config(&cache_path))
    else {
        panic!("expected a cache directory creation error");
    };
    assert!(message.contains("Failed to create cache directory"));
    assert!(message.contains("not-a-directory"));
    assert_eq!(std::fs::read(&cache_path).unwrap(), b"keep me");
}

#[tokio::test]
async fn media_bytes_use_default_image_hint_and_classify_video_and_documents() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(
            module_path!(),
            "::media_bytes_use_default_image_hint_and_classify_video_and_documents"
        );
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let fixture = tempfile::tempdir().unwrap();
    let cache = MediaCache::new(&cache_config(fixture.path())).unwrap();

    for name in ["no-extension", "unknown.blob"] {
        assert!(cache
            .cache_media_bytes(b"unsupported", Some(name), None, Some("video"))
            .await
            .unwrap()
            .is_none());
    }
    assert!(std::fs::read_dir(fixture.path()).unwrap().next().is_none());

    for (data, name, mime, hint, kind, expected_mime) in [
        (
            b"\x89PNG\r\n\x1a\n".as_slice(),
            "cover",
            Some(" \t "),
            Some("image"),
            "image",
            "image/png",
        ),
        (
            b"video bytes".as_slice(),
            "clip.MOV",
            None,
            None,
            "video",
            "video/quicktime",
        ),
        (
            b"%PDF-1.4".as_slice(),
            "report.PDF",
            Some(" Application/Octet-Stream "),
            None,
            "document",
            "application/octet-stream",
        ),
    ] {
        let media = cache
            .cache_media_bytes(data, Some(name), mime, hint)
            .await
            .unwrap()
            .expect("supported media");
        assert_eq!(media.display_name, name);
        assert_eq!(media.kind, kind);
        assert_eq!(media.media_type, expected_mime);
        assert_eq!(
            media.path.parent(),
            Some(fixture.path().join(kind).as_path())
        );
        assert_eq!(fs::read(&media.path).await.unwrap(), data);
    }
}

#[tokio::test]
async fn byte_cache_counts_nested_files_and_allows_an_exact_fit() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(
            module_path!(),
            "::byte_cache_counts_nested_files_and_allows_an_exact_fit"
        );
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let fixture = tempfile::tempdir().unwrap();
    let mut config = cache_config(fixture.path());
    config.max_size = 5;
    let cache = MediaCache::new(&config).unwrap();
    let nested = fixture.path().join("existing/archive");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(nested.join("seed"), b"ab").unwrap();

    let path = cache
        .write_bytes_to_cache("documents", b"cde", "fits.txt")
        .await
        .unwrap();
    assert_eq!(fs::read(&path).await.unwrap(), b"cde");

    let error = cache
        .write_bytes_to_cache("images", b"f", "over.png")
        .await
        .unwrap_err();
    let message = error.to_string();
    assert!(message.contains("Cache size limit exceeded while caching over.png"));
    assert!(message.contains("current=5, incoming=1, max=5"));
    assert!(!fixture.path().join("images").exists());
    assert_eq!(cache.calculate_cache_size().await.unwrap(), 5);
}

#[tokio::test]
async fn byte_cache_distinguishes_directory_and_file_creation_errors() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(
            module_path!(),
            "::byte_cache_distinguishes_directory_and_file_creation_errors"
        );
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let fixture = tempfile::tempdir().unwrap();
    let cache = MediaCache::new(&cache_config(fixture.path())).unwrap();
    let blocked = fixture.path().join("documents");
    std::fs::write(&blocked, b"keep me").unwrap();

    let error = cache
        .cache_document_from_bytes(b"text", Some("note.txt"))
        .await
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("Failed to create cache subdirectory"));
    assert_eq!(fs::read(&blocked).await.unwrap(), b"keep me");

    let too_long = "x".repeat(4096);
    let error = cache
        .write_bytes_to_cache("audio", b"sound", &too_long)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("Failed to create file"));
    assert!(std::fs::read_dir(fixture.path().join("audio"))
        .unwrap()
        .next()
        .is_none());
}

#[tokio::test]
async fn audio_cache_reuses_local_files_according_to_ttl() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(
            module_path!(),
            "::audio_cache_reuses_local_files_according_to_ttl"
        );
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    // A numeric host needs no DNS lookup. The HTTPS-only client rejects this
    // HTTP URL before connecting if a cache lookup falls through to send().
    const URL: &str = "http://8.8.8.8/clip.ogg";

    let fixture = tempfile::tempdir().unwrap();
    let mut config = cache_config(fixture.path());
    config.ttl_seconds = 3600;
    let mut cache = MediaCache::new(&config).unwrap();
    cache.client = reqwest::Client::builder()
        .https_only(true)
        .no_proxy()
        .build()
        .unwrap();

    let audio_dir = fixture.path().join("audio");
    std::fs::write(&audio_dir, b"not a directory").unwrap();
    let error = cache.cache_audio(URL, "clip.ogg").await.unwrap_err();
    assert!(error
        .to_string()
        .contains("Failed to create cache subdirectory"));
    assert_eq!(fs::read(&audio_dir).await.unwrap(), b"not a directory");

    std::fs::remove_file(&audio_dir).unwrap();
    std::fs::create_dir(&audio_dir).unwrap();
    let clip = audio_dir.join("clip.ogg");
    let now = SystemTime::now();
    write_at(&clip, b"fresh fixture", now - Duration::from_secs(60));
    assert_eq!(cache.cache_audio(URL, " clip.ogg ").await.unwrap(), clip);
    assert_eq!(fs::read(&clip).await.unwrap(), b"fresh fixture");

    write_at(&clip, b"old fixture", now - Duration::from_secs(7200));
    let error = cache.cache_audio(URL, "clip.ogg").await.unwrap_err();
    assert!(error.to_string().contains("Failed to download"));
    assert_eq!(fs::read(&clip).await.unwrap(), b"old fixture");

    cache.ttl_seconds = 0;
    assert_eq!(cache.cache_audio(URL, "clip.ogg").await.unwrap(), clip);
    assert_eq!(fs::read(&clip).await.unwrap(), b"old fixture");
}

#[tokio::test]
async fn cleanup_removes_only_expired_files_and_updates_the_recursive_size() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(
            module_path!(),
            "::cleanup_removes_only_expired_files_and_updates_the_recursive_size"
        );
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let fixture = tempfile::tempdir().unwrap();
    let cache = MediaCache::new(&cache_config(fixture.path())).unwrap();
    let nested = fixture.path().join("documents/archive");
    std::fs::create_dir_all(&nested).unwrap();
    let old_root = fixture.path().join("old.txt");
    let old_nested = nested.join("old.txt");
    let recent = nested.join("recent.txt");
    let now = SystemTime::now();
    let old_time = now - Duration::from_secs(7200);
    write_at(&old_root, b"one", old_time);
    write_at(&old_nested, b"two", old_time);
    write_at(&recent, b"stay", now - Duration::from_secs(60));

    assert_eq!(cache.cleanup_expired(0).await.unwrap(), 0);
    assert!(old_root.is_file());
    assert!(old_nested.is_file());
    assert_eq!(cache.calculate_cache_size().await.unwrap(), 10);

    assert_eq!(cache.cleanup_expired(3600).await.unwrap(), 2);
    assert!(!old_root.exists());
    assert!(!old_nested.exists());
    assert_eq!(fs::read(&recent).await.unwrap(), b"stay");
    assert_eq!(cache.calculate_cache_size().await.unwrap(), 4);
    assert_eq!(cache.cleanup_expired(3600).await.unwrap(), 0);
}

#[tokio::test]
async fn cache_traversal_reports_a_root_replaced_by_a_regular_file() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(
            module_path!(),
            "::cache_traversal_reports_a_root_replaced_by_a_regular_file"
        );
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("cache");
    let cache = MediaCache::new(&cache_config(&root)).unwrap();
    std::fs::remove_dir(&root).unwrap();
    std::fs::write(&root, b"keep me").unwrap();

    let cleanup_error = cache.cleanup_expired(3600).await.unwrap_err();
    assert!(cleanup_error
        .to_string()
        .contains("Failed to read directory"));
    let size_error = cache.calculate_cache_size().await.unwrap_err();
    assert!(size_error.to_string().contains("Failed to read directory"));
    assert_eq!(fs::read(&root).await.unwrap(), b"keep me");
}

#[test]
fn blank_file_names_are_rejected() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::blank_file_names_are_rejected");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let Err(GatewayError::ConnectionFailed(message)) = sanitize_file_name(" \t\n ") else {
        panic!("expected a connection error for a blank file name");
    };
    assert_eq!(message, "File name must not be empty");
}

#[cfg(unix)]
#[test]
fn safe_leaf_replaces_a_literal_backslash_on_unix() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(
            module_path!(),
            "::safe_leaf_replaces_a_literal_backslash_on_unix"
        );
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    assert_eq!(
        safe_leaf_name("uploads/part\\name\0.pdf", "document"),
        "part_name.pdf"
    );
}

#[test]
fn mime_lookup_accepts_extensions_without_a_leading_dot() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(
            module_path!(),
            "::mime_lookup_accepts_extensions_without_a_leading_dot"
        );
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    assert_eq!(mime_from_extension("JpEG"), Some("image/jpeg"));
    assert_eq!(mime_from_extension("PdF"), Some("application/pdf"));
    assert_eq!(mime_from_extension("not-a-media-extension"), None);
}

#[test]
fn image_detection_validates_signatures_and_requires_nonempty_unknown_types() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(
            module_path!(),
            "::image_detection_validates_signatures_and_requires_nonempty_unknown_types"
        );
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    assert!(looks_like_image(b"\xff\xd8\xff", "image/jpeg"));
    assert!(!looks_like_image(b"\xff\xd8", "image/jpeg"));

    assert!(looks_like_image(b"GIF87a", "image/gif"));
    assert!(looks_like_image(b"GIF89a", "image/gif"));
    assert!(!looks_like_image(b"GIF90a", "image/gif"));

    assert!(looks_like_image(b"RIFF0000WEBP", "image/webp"));
    assert!(!looks_like_image(b"RIFF0000WAVE", "image/webp"));
    assert!(!looks_like_image(b"RIFX0000WEBP", "image/webp"));
    assert!(!looks_like_image(b"RIFF", "image/webp"));

    assert!(looks_like_image(b"nonempty", "image/x-custom"));
    assert!(!looks_like_image(b"", "image/x-custom"));
}
