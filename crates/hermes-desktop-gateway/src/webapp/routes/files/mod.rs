//! Host folders, file bytes and repository state for the Webapp's file picker, attachments and
//! coding rail: `/api/fs/list`, `/api/fs/read-data-url`, `/api/git/status` and
//! `/api/git/review/rev-parse`.
//!
//! Upstream at the pin: `hermes_cli/web_routers/files.py` and `web_routers/git.py` with
//! `hermes_cli/web_git.py`. Stricter than upstream, each on purpose:
//! - a path must resolve inside the user's home folder or the served profile's working folder
//!   (upstream: the whole host on a loopback Webapp), and the Hermes home is refused wherever it
//!   is (it holds the browser credential and saved gateway tokens, which upstream's list lacks);
//!   a home or working folder that is `/`, holds a system folder, or sits in `/proc`, `/sys`,
//!   `/dev`, `/run`, `/etc`, `/root` or `/var` (bar a home under `/var/home/<user>`) is not a
//!   root;
//! - credential files and folders are refused anywhere, browser profiles, cookie and password
//!   stores and keyrings included;
//! - paths are absolute or `~`; the resolved path is opened by walking it from the root's
//!   descriptor without following links, so a link swapped in after the check is refused;
//! - a path outside the roots answers 403 whether or not it exists or is readable;
//! - only a loopback peer reaches these routes, and never the remote-mode token caller;
//! - `rev-parse` takes a ref of plain ref characters only, after `--end-of-options`;
//! - git never searches above the root, never enters a submodule, runs no hook, external diff or
//!   configured program, and refuses any repository whose work tree or git folder leaves the root,
//!   enters a refused or sensitive tree, or points away from the checked folder; on Linux it runs
//!   in the folder descriptor that was checked (elsewhere by its checked path, so a link swapped
//!   in during the call is a limitation).

use std::collections::HashMap;
use std::path::PathBuf;

use axum::extract::Query;
use axum::http::{header, Extensions, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use base64::Engine as _;
use serde_json::{json, Value};

use hermes_core::git::GitRead;

use super::startup::default_cwd_for;
use super::store::{blocking, Served};
use crate::webapp::api::{served_profile, ApiError, ApiResult, Ctx};

mod fs;
mod git;
mod roots;

use fs::{etag, list_entries, mime_type, open_resolved, read_capped, Opened, DATA_URL_MAX_BYTES};
use git::{git_line, repo_dir, repo_status, repo_top, rev_parse_args, valid_ref};
use roots::{local_only, peer, resolve, roots_for, Refusal, Resolved, Roots};

/// Each handler gets `served` from the route, not from the request; the requested path reaches
/// the file system only through [`resolve`].
pub fn router(served: &Served) -> Router {
    let (list, read, status, rev) = (
        served.clone(),
        served.clone(),
        served.clone(),
        served.clone(),
    );
    Router::new()
        .route(
            "/api/fs/list",
            get(
                move |ctx: Ctx, ext: Extensions, q: Query<HashMap<String, String>>| {
                    fs_list(list.clone(), ctx, ext, q)
                },
            ),
        )
        .route(
            "/api/fs/read-data-url",
            get(
                move |ctx: Ctx,
                      ext: Extensions,
                      headers: HeaderMap,
                      q: Query<HashMap<String, String>>| {
                    read_data_url(read.clone(), ctx, ext, headers, q)
                },
            ),
        )
        .route(
            "/api/git/status",
            get(
                move |ctx: Ctx, ext: Extensions, q: Query<HashMap<String, String>>| {
                    git_status(status.clone(), ctx, ext, q)
                },
            ),
        )
        .route(
            "/api/git/review/rev-parse",
            get(
                move |ctx: Ctx, ext: Extensions, q: Query<HashMap<String, String>>| {
                    rev_parse(rev.clone(), ctx, ext, q)
                },
            ),
        )
}

fn admit(
    served: &Served,
    ctx: &Ctx,
    ext: &Extensions,
    q: &HashMap<String, String>,
) -> Result<(Roots, Option<PathBuf>), ApiError> {
    local_only(ctx.caller, peer(ext))?;
    served_profile(&ctx.api, q.get("profile").map(String::as_str))?;
    let hermes_home = served.home().to_path_buf();
    let user_home = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from);
    let cwd = default_cwd_for(&hermes_home);
    let refused = [hermes_home, served.install_root()];
    Ok((roots_for(user_home.as_deref(), &cwd, &refused), user_home))
}

fn path_param(q: &HashMap<String, String>) -> Result<String, ApiError> {
    q.get("path")
        .filter(|p| !p.trim().is_empty())
        .cloned()
        .ok_or_else(|| ApiError(StatusCode::BAD_REQUEST, "path is required".into()))
}

async fn fs_list(
    served: Served,
    ctx: Ctx,
    ext: Extensions,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult {
    let (roots, home) = admit(&served, &ctx, &ext, &q)?;
    let raw = path_param(&q)?;
    blocking(move || {
        listing(
            resolve(&raw, &roots, home.as_deref()).and_then(|r| {
                let opened = open_resolved(&r)?;
                Ok((r, opened))
            }),
            &roots.refused,
        )
    })
    .await
    .map(Json)
}

/// `fs/list`'s answer for a checked and opened path: a missing, unreadable or non-folder path
/// answers as upstream's does, in the body; any other refusal is the request's error.
pub(crate) fn listing(
    opened: Result<(Resolved, Opened), Refusal>,
    refused: &[PathBuf],
) -> Result<Value, ApiError> {
    let error = |code: &str| Ok(json!({"entries": [], "error": code}));
    match opened {
        Ok((resolved, Opened::Dir(dir))) => Ok(match list_entries(dir, &resolved.path, refused) {
            Ok((entries, false)) => json!({"entries": entries}),
            Ok((entries, true)) => json!({"entries": entries, "truncated": true}),
            Err(_) => json!({"entries": [], "error": "read-error"}),
        }),
        Ok((_, Opened::File(..))) | Err(Refusal::NotADirectory | Refusal::NotAFile) => {
            error("ENOTDIR")
        }
        Err(Refusal::NotFound) => error("ENOENT"),
        Err(Refusal::Denied) => error("EACCES"),
        Err(other) => Err(other.into()),
    }
}

/// `read-data-url`'s answer for a checked path: the file's ETag, and its data URL unless `wanted`
/// holds that ETag. A file over the limit is refused before it is read.
pub(crate) fn data_url(
    resolved: &Resolved,
    wanted: &[String],
) -> Result<(String, Option<String>), ApiError> {
    let (file, st) = match open_resolved(resolved)? {
        Opened::File(file, st) => (file, st),
        Opened::Dir(_) => return Err(Refusal::NotAFile.into()),
    };
    if st.st_size as u64 > DATA_URL_MAX_BYTES {
        return Err(Refusal::TooLarge.into());
    }
    let tag = etag(&resolved.path, &st);
    if wanted.iter().any(|w| *w == tag || w == "*") {
        return Ok((tag, None));
    }
    let bytes = read_capped(file, DATA_URL_MAX_BYTES)?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    Ok((
        tag,
        Some(format!(
            "data:{};base64,{encoded}",
            mime_type(&resolved.path)
        )),
    ))
}

async fn read_data_url(
    served: Served,
    ctx: Ctx,
    ext: Extensions,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let work = async {
        let (roots, home) = admit(&served, &ctx, &ext, &q)?;
        let raw = path_param(&q)?;
        let wanted: Vec<String> = headers
            .get(header::IF_NONE_MATCH)
            .and_then(|v| v.to_str().ok())
            .map(|v| {
                v.split(',')
                    .map(|t| t.trim().trim_start_matches("W/").to_owned())
                    .collect()
            })
            .unwrap_or_default();
        blocking(move || {
            let resolved = resolve(&raw, &roots, home.as_deref())?;
            data_url(&resolved, &wanted)
        })
        .await
    };
    match work.await {
        Ok((tag, body)) => {
            let mut response = match body {
                Some(url) => Json(json!({"dataUrl": url})).into_response(),
                None => StatusCode::NOT_MODIFIED.into_response(),
            };
            let h = response.headers_mut();
            h.insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("private, no-cache"),
            );
            h.extend(HeaderValue::from_str(&tag).ok().map(|t| (header::ETAG, t)));
            response
        }
        Err(err) => err.into_response(),
    }
}

async fn git_status(
    served: Served,
    ctx: Ctx,
    ext: Extensions,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult {
    let (roots, home) = admit(&served, &ctx, &ext, &q)?;
    let raw = path_param(&q)?;
    blocking(move || {
        let dir = match repo_dir(&raw, &roots, home.as_deref()) {
            Ok(dir) => dir,
            Err(ApiError(status, _))
                if status == StatusCode::NOT_FOUND || status == StatusCode::BAD_REQUEST =>
            {
                return Ok(Value::Null)
            }
            Err(err) => return Err(err),
        };
        Ok(repo_status(&dir, &roots.refused).unwrap_or(Value::Null))
    })
    .await
    .map(Json)
}

async fn rev_parse(
    served: Served,
    ctx: Ctx,
    ext: Extensions,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult {
    let (roots, home) = admit(&served, &ctx, &ext, &q)?;
    let raw = path_param(&q)?;
    let wanted = q
        .get("ref")
        .filter(|r| !r.is_empty())
        .cloned()
        .unwrap_or_else(|| "HEAD".into());
    if !valid_ref(&wanted) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "Not a ref".into()));
    }
    blocking(move || {
        let dir = repo_dir(&raw, &roots, home.as_deref())?;
        let git_path = dir.git_path();
        let sha = repo_top(&git_path, &dir.resolved.root, &roots.refused).and_then(|_| {
            git_line(
                &git_path,
                &dir.resolved.root,
                GitRead::RevParse,
                &rev_parse_args(&wanted),
            )
        });
        Ok(json!({"sha": sha}))
    })
    .await
    .map(Json)
}

#[cfg(test)]
mod tests;
