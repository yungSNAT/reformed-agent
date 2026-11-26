//! The Webapp bundle and how its files are served, confined to the bundle's root.
//!
//! The bundle is packaged only: `HERMES_WEBAPP_DIST` names a prebuilt folder holding
//! `index.html`. This build ships no renderer sources and never builds one (upstream builds its
//! own at `<project root>/apps/desktop/dist-webapp` when the variable is unset).
//!
//! Every file is opened through descriptors rooted at the bundle's folder, one path segment at a
//! time with `O_NOFOLLOW`, after refusing empty, `.`, `..` and dot segments: a symbolic link
//! anywhere below the root refuses the request, so no swap can lead a read outside it. The root
//! itself may be a link the operator chose. Bytes come only from the descriptor opened.

use std::ffi::CString;
use std::fs::File;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::Engine;
use serde_json::json;
use sha2::{Digest, Sha256};

/// The variable naming the packaged bundle.
pub const DIST_ENV: &str = "HERMES_WEBAPP_DIST";
/// A bundle `hermes update` could not rebuild (upstream `STALE_MARKER`).
pub const STALE_MARKER: &str = ".hermes-stale";
/// How every missing-bundle reason starts (upstream `NOT_BUILT`).
pub const NOT_BUILT: &str = "The Webapp is not built";
/// Largest file served.
const MAX_FILE: u64 = 64 << 20;

/// The bundle folder `HERMES_WEBAPP_DIST` names (a leading `~/` is the home folder), if set.
pub fn dist_from_env() -> Option<PathBuf> {
    let value = std::env::var_os(DIST_ENV).filter(|v| !v.is_empty())?;
    let text = value.to_string_lossy();
    match text.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME").map(|home| Path::new(&home).join(rest)),
        None => Some(PathBuf::from(value)),
    }
}

/// Why the Webapp cannot be served from `dist`, or None once it can.
pub fn missing_reason(dist: Option<&Path>) -> Option<String> {
    let Some(dist) = dist else {
        return Some(format!(
            "{NOT_BUILT}: set {DIST_ENV} to a prebuilt Webapp bundle (a folder holding index.html); \
             this build does not build one"
        ));
    };
    let index_ok = open_root(dist)
        .and_then(|root| open_beneath(&root, &["index.html"]))
        .and_then(|file| file.metadata())
        .is_ok_and(|meta| meta.is_file());
    if !index_ok {
        return Some(format!(
            "{NOT_BUILT} ({} is missing). Set {DIST_ENV} to a prebuilt Webapp bundle.",
            dist.join("index.html").display()
        ));
    }
    if stale(dist) {
        return Some(format!(
            "{NOT_BUILT} for this version of Hermes: {} is marked out of date ({STALE_MARKER}). \
             Replace it with a current bundle.",
            dist.display()
        ));
    }
    None
}

fn stale(dist: &Path) -> bool {
    open_root(dist)
        .and_then(|root| open_beneath(&root, &[STALE_MARKER]))
        .is_ok()
}

fn open_root(dist: &Path) -> std::io::Result<File> {
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
        .open(dist)
}

/// Opens `segments` below the directory `root`, refusing a symbolic link at every step.
fn open_beneath(root: &File, segments: &[&str]) -> std::io::Result<File> {
    let refused = || std::io::Error::from(std::io::ErrorKind::NotFound);
    let (last, dirs) = segments.split_last().ok_or_else(refused)?;
    let mut owned: Option<File> = None;
    for (i, segment) in dirs.iter().chain(std::iter::once(last)).enumerate() {
        let name = CString::new(*segment).map_err(|_| refused())?;
        let flags = if i < dirs.len() {
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC
        } else {
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC
        };
        let dir = owned.as_ref().unwrap_or(root);
        // SAFETY: openat reads the NUL-terminated name and returns a new descriptor or -1.
        let fd = unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: `fd` is a descriptor this call just opened and nothing else owns.
        owned = Some(unsafe { File::from_raw_fd(fd) });
    }
    owned.ok_or_else(refused)
}

/// The request path's segments, decoded once; None for a path that may not be served (an empty,
/// `.`, `..` or dot segment, or one holding `\`, `:` or NUL, or text that is not UTF-8).
fn segments(path: &str) -> Option<Vec<String>> {
    let decoded = percent_encoding::percent_decode_str(path)
        .decode_utf8()
        .ok()?;
    let rest = decoded.strip_prefix('/').unwrap_or(&decoded);
    if rest.is_empty() {
        return Some(Vec::new());
    }
    rest.split('/')
        .map(|s| {
            let ok = !s.is_empty() && !s.starts_with('.') && !s.contains(['\\', ':', '\0']);
            ok.then(|| s.to_string())
        })
        .collect()
}

fn has_extension(path: &str) -> bool {
    path.rsplit('/')
        .next()
        .is_some_and(|last| last.contains('.'))
}

/// What a page route answers.
pub struct Page {
    pub status: StatusCode,
    pub content_type: &'static str,
    pub body: Vec<u8>,
    /// The entry document: served with the bootstrap and its own policy.
    pub entry: bool,
}

/// What the bootstrap names: the profile this gateway serves.
pub struct Bootstrap<'a> {
    pub profile: &'a str,
}

fn not_found(detail: String) -> Result<Page, Response> {
    Err((StatusCode::NOT_FOUND, Json(json!({"detail": detail}))).into_response())
}

fn unavailable(status: StatusCode, detail: String) -> Response {
    (status, Json(json!({"detail": detail}))).into_response()
}

/// Serves `path` from the bundle at `dist` (blocking file reads; call it off the async threads).
pub fn serve(dist: Option<&Path>, path: &str, boot: &Bootstrap) -> Response {
    match resolve(dist, path, boot) {
        Ok(page) => respond(page),
        Err(response) => response,
    }
}

fn resolve(dist: Option<&Path>, path: &str, boot: &Bootstrap) -> Result<Page, Response> {
    if path == "/api" || path.starts_with("/api/") {
        return not_found(format!("No such API endpoint: {path}"));
    }
    if let Some(reason) = missing_reason(dist) {
        let status = if reason.contains(STALE_MARKER) {
            StatusCode::SERVICE_UNAVAILABLE
        } else {
            StatusCode::NOT_FOUND
        };
        return Err(unavailable(status, reason));
    }
    let dist = dist.expect("missing_reason refuses an unset bundle");
    let Some(segments) = segments(path) else {
        return not_found("not found".into());
    };
    let root = open_root(dist).map_err(|e| unavailable(StatusCode::NOT_FOUND, e.to_string()))?;
    let index = open_beneath(&root, &["index.html"])
        .and_then(|f| f.metadata())
        .map_err(|e| unavailable(StatusCode::NOT_FOUND, e.to_string()))?;
    let refs: Vec<&str> = segments.iter().map(String::as_str).collect();
    let file = if refs.is_empty() {
        None
    } else {
        open_beneath(&root, &refs)
            .ok()
            .filter(|f| f.metadata().is_ok_and(|m| m.is_file()))
    };
    match file {
        Some(file) => {
            let meta = file
                .metadata()
                .map_err(|e| unavailable(StatusCode::NOT_FOUND, e.to_string()))?;
            if (meta.dev(), meta.ino()) == (index.dev(), index.ino()) {
                return entry(&root, boot);
            }
            if meta.len() > MAX_FILE {
                return not_found("not found".into());
            }
            let mut body = Vec::with_capacity(meta.len() as usize);
            file.take(MAX_FILE)
                .read_to_end(&mut body)
                .map_err(|e| unavailable(StatusCode::NOT_FOUND, e.to_string()))?;
            Ok(Page {
                status: StatusCode::OK,
                content_type: content_type(refs.last().copied().unwrap_or_default()),
                body,
                entry: false,
            })
        }
        None if has_extension(path) => not_found("not found".into()),
        None => entry(&root, boot),
    }
}

fn entry(root: &File, boot: &Bootstrap) -> Result<Page, Response> {
    let mut html = String::new();
    open_beneath(root, &["index.html"])
        .and_then(|f| f.take(MAX_FILE).read_to_string(&mut html))
        .map_err(|e| unavailable(StatusCode::NOT_FOUND, e.to_string()))?;
    let html = html.trim_start_matches('\u{feff}');
    Ok(Page {
        status: StatusCode::OK,
        content_type: "text/html; charset=utf-8",
        body: with_bootstrap(html, boot).into_bytes(),
        entry: true,
    })
}

/// A JSON string literal that cannot close a script element.
fn script_string(text: &str) -> String {
    serde_json::to_string(text)
        .expect("a string serialises")
        .replace("</", "<\\/")
}

/// The entry document with upstream's Webapp bootstrap before `</head>`: never a credential.
pub fn with_bootstrap(html: &str, boot: &Bootstrap) -> String {
    let script = format!(
        "<script>window.__HERMES_DASHBOARD_EMBEDDED_CHAT__=true;\
         window.__HERMES_BASE_PATH__=\"\";\
         window.__HERMES_AUTH_REQUIRED__=false;\
         window.__HERMES_INITIAL_PROFILE__=\"\";\
         window.__HERMES_UI_SURFACE__=\"webapp\";\
         window.__HERMES_DASHBOARD_PROFILE__={};</script>",
        script_string(boot.profile)
    );
    match html.find("</head>") {
        Some(at) => format!("{}{script}{}", &html[..at], &html[at..]),
        None => format!("{script}{html}"),
    }
}

/// sha256 sources for every inline script of `html` (upstream webapp_page_policy.py).
pub fn script_hashes(html: &str) -> Vec<String> {
    let lower = html.to_ascii_lowercase();
    let mut hashes = Vec::new();
    let mut from = 0;
    while let Some(start) = lower[from..].find("<script").map(|i| i + from) {
        let after = start + "<script".len();
        let Some(close) = lower[after..].find('>').map(|i| i + after) else {
            break;
        };
        let attrs = &lower[after..close];
        let boundary = attrs
            .chars()
            .next()
            .is_none_or(|c| c.is_ascii_whitespace() || c == '/');
        if !boundary {
            from = after;
            continue;
        }
        let Some(end) = lower[close + 1..].find("</script").map(|i| i + close + 1) else {
            break;
        };
        let has_src = attrs
            .split(|c: char| c.is_ascii_whitespace() || c == '/')
            .any(|attr| attr == "src" || attr.starts_with("src="));
        if !has_src {
            let digest = Sha256::digest(html[close + 1..end].as_bytes());
            hashes.push(format!(
                "'sha256-{}'",
                base64::engine::general_purpose::STANDARD.encode(digest)
            ));
        }
        from = end;
    }
    hashes
}

/// Upstream's Webapp page policy, with `html`'s inline-script hashes.
pub fn page_policy(html: &str) -> String {
    let hashes = script_hashes(html).join(" ");
    [
        "default-src 'none'".to_string(),
        format!("script-src 'self' blob: 'wasm-unsafe-eval' {hashes}"),
        "script-src-attr 'none'".into(),
        "style-src 'self' 'unsafe-inline' https://fonts.googleapis.com".into(),
        "font-src 'self' data: https://fonts.gstatic.com".into(),
        "connect-src * ws: wss: data: blob:".into(),
        "img-src 'self' data: blob: https: http:".into(),
        "media-src * data: blob:".into(),
        "worker-src 'self' blob:".into(),
        "frame-src 'self' blob: data: https: http:".into(),
        "manifest-src 'self'".into(),
        "object-src 'none'".into(),
        "base-uri 'none'".into(),
        "form-action 'none'".into(),
        "frame-ancestors 'none'".into(),
    ]
    .join("; ")
}

fn respond(page: Page) -> Response {
    let policy = page
        .entry
        .then(|| page_policy(&String::from_utf8_lossy(&page.body)));
    let mut response = (page.status, page.body).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(page.content_type),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    if let Some(policy) = policy {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        if let Ok(value) = HeaderValue::from_str(&policy) {
            headers.insert(header::CONTENT_SECURITY_POLICY, value);
        }
    }
    response
}

fn content_type(name: &str) -> &'static str {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
    match ext.as_deref() {
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json" | "map") => "application/json",
        Some("webmanifest") => "application/manifest+json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("avif") => "image/avif",
        Some("ico") => "image/x-icon",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        Some("otf") => "font/otf",
        Some("wasm") => "application/wasm",
        Some("txt") => "text/plain; charset=utf-8",
        Some("mp3") => "audio/mpeg",
        Some("wav") => "audio/wav",
        Some("ogg") => "audio/ogg",
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_are_decoded_once_and_unsafe_ones_refused() {
        assert_eq!(segments("/").unwrap(), Vec::<String>::new());
        assert_eq!(segments("/assets/a.js").unwrap(), ["assets", "a.js"]);
        assert_eq!(segments("/%61ssets/a.js").unwrap(), ["assets", "a.js"]);
        for bad in [
            "/../x",
            "/a/../x",
            "/%2e%2e/x",
            "/a/./x",
            "/a//x",
            "/.env",
            "/a%2f..%2fx",
            "/a%5cb",
            "/c:x",
            "/a%00b",
            "/%ff",
        ] {
            assert!(segments(bad).is_none(), "{bad}");
        }
        // Decoded once: %252e stays the literal text %2e.
        assert_eq!(segments("/%252e%252e").unwrap(), ["%2e%2e"]);
    }

    #[test]
    fn inline_scripts_are_hashed_and_external_ones_skipped() {
        let html = "<head><script>a()</script><SCRIPT type=module src=/x.js></SCRIPT>\
                    <script\n>b()</script><scripts>c()</scripts></head>";
        let hashes = script_hashes(html);
        assert_eq!(hashes.len(), 2, "{hashes:?}");
        let expected = base64::engine::general_purpose::STANDARD.encode(Sha256::digest(b"a()"));
        assert_eq!(hashes[0], format!("'sha256-{expected}'"));
    }

    #[test]
    fn the_bootstrap_cannot_close_its_script() {
        let html = with_bootstrap(
            "<head></head>",
            &Bootstrap {
                profile: "</script><x>",
            },
        );
        assert!(html.contains(r#""<\/script><x>""#), "{html}");
        assert!(html.ends_with("</head>"));
    }

    #[test]
    fn a_link_swapped_in_while_serving_never_leaks_outside_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let dist = dir.path().join("dist");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(dist.join("assets")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(dist.join("index.html"), "<html></html>").unwrap();
        std::fs::write(dist.join("assets/file.js"), "inside").unwrap();
        std::fs::write(outside.join("file.js"), "OUTSIDE").unwrap();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let swapper = {
            let (dist, outside, stop) = (dist.clone(), outside.clone(), stop.clone());
            std::thread::spawn(move || {
                let assets = dist.join("assets");
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    // The file, then the whole folder, briefly become links out of the root.
                    let _ =
                        std::os::unix::fs::symlink(outside.join("file.js"), assets.join("swap"));
                    let _ = std::fs::rename(assets.join("swap"), assets.join("file.js"));
                    let _ = std::fs::write(assets.join("real"), "inside");
                    let _ = std::fs::rename(assets.join("real"), assets.join("file.js"));
                    let _ = std::fs::rename(&assets, dist.join("held"));
                    let _ = std::os::unix::fs::symlink(&outside, &assets);
                    let _ = std::fs::remove_file(&assets);
                    let _ = std::fs::rename(dist.join("held"), &assets);
                }
            })
        };
        let boot = Bootstrap { profile: "default" };
        let mut served = 0;
        for _ in 0..1000 {
            if let Ok(page) = resolve(Some(&dist), "/assets/file.js", &boot) {
                assert_ne!(page.body, b"OUTSIDE");
                served += 1;
            }
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        swapper.join().unwrap();
        assert!(served > 0, "no request was served while the swaps ran");
    }

    #[test]
    fn a_missing_or_stale_bundle_is_a_clear_error() {
        let dir = tempfile::tempdir().unwrap();
        let boot = Bootstrap { profile: "default" };
        let missing = resolve(Some(dir.path()), "/", &boot).err().unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
        assert!(missing_reason(Some(dir.path()))
            .unwrap()
            .contains("index.html is missing"));
        assert!(missing_reason(None).unwrap().contains(DIST_ENV));
        std::fs::write(dir.path().join("index.html"), "<html></html>").unwrap();
        assert!(missing_reason(Some(dir.path())).is_none());
        std::fs::write(dir.path().join(STALE_MARKER), "x").unwrap();
        let stale = resolve(Some(dir.path()), "/", &boot).err().unwrap();
        assert_eq!(stale.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
