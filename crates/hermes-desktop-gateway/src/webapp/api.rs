//! The shape every REST route group builds on: who the guard admitted ([`Caller`]), the state a
//! handler may read ([`Api`]), the extractor that carries both ([`Ctx`]) and the group list
//! ([`GROUPS`]) that `router()` merges behind the credential guard.
//!
//! A group is one file `src/webapp/routes/<group>.rs` with `pub fn router() -> Router` and
//! handlers such as `async fn get_config(ctx: Ctx, Query(q): Query<HashMap<String, String>>) ->
//! ApiResult`, plus one line in [`GROUPS`]. Groups add no auth layer of their own and never see a
//! credential: [`Ctx`] exists only for a request the guard admitted.

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use serde_json::{json, Value};

use crate::Chat;

/// Who the credential guard admitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Caller {
    /// Remote mode: the session token.
    Token,
    /// Local mode: a spent `native-http` ticket, with no `Origin`.
    Native,
    /// Local mode with the Webapp serving: the browser credential of this generation.
    Browser { generation: u64 },
}

/// What every REST handler may read.
pub struct Api {
    pub chat: Arc<Chat>,
    /// The Hermes home this gateway serves; None in remote mode.
    pub home: Option<PathBuf>,
    /// The one profile this gateway serves.
    pub profile: String,
}

/// An admitted request: the shared state and who is calling. Built from the request's
/// extensions, which only the credential guard fills, so a route outside the guard cannot build
/// one and answers 500 rather than serve an unauthenticated caller.
pub struct Ctx {
    pub api: Arc<Api>,
    pub caller: Caller,
}

impl<S: Send + Sync> FromRequestParts<S> for Ctx {
    type Rejection = StatusCode;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let caller = parts.extensions.get::<Caller>().copied();
        let api = parts.extensions.get::<Arc<Api>>().cloned();
        match (api, caller) {
            (Some(api), Some(caller)) => Ok(Ctx { api, caller }),
            _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
        }
    }
}

/// A JSON error as upstream's server gives one: `{"detail": "..."}` with the status.
#[derive(Debug)]
pub struct ApiError(pub StatusCode, pub String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"detail": self.1}))).into_response()
    }
}

pub type ApiResult<T = Value> = Result<Json<T>, ApiError>;

/// One group of routes.
pub type Group = fn() -> Router;

/// Every group, merged once behind the credential guard. One line per group.
pub const GROUPS: &[Group] = &[];

/// Checks the profile a request names (`?profile=`) against the one this gateway serves: absent
/// or equal is Ok; any other name is 404, as this gateway serves one profile.
pub fn served_profile(api: &Api, requested: Option<&str>) -> Result<(), ApiError> {
    match requested {
        None | Some("") => Ok(()),
        Some(name) if name == api.profile => Ok(()),
        Some(name) => Err(ApiError(
            StatusCode::NOT_FOUND,
            format!("profile {name} is not served here"),
        )),
    }
}

/// The groups as one router, ready to merge into a router of any state.
pub(crate) fn groups<S: Clone + Send + Sync + 'static>() -> Router<S> {
    GROUPS
        .iter()
        .fold(Router::new(), |router, group| router.merge(group()))
        .with_state(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::Request;
    use axum::middleware::{self, Next};
    use axum::routing::get;

    fn native_routes() -> Router {
        Router::new().route("/native", get(|| async { "native" }))
    }

    fn shared_routes() -> Router {
        Router::new().route("/shared", get(|| async { "shared" }))
    }

    /// Stands in for the credential guard: admits the caller the request names.
    async fn admit_as(mut request: Request, next: Next) -> Response {
        let caller = match request.headers().get("x-caller").and_then(|v| v.to_str().ok()) {
            Some("browser") => Some(Caller::Browser { generation: 1 }),
            Some("native") => Some(Caller::Native),
            Some("token") => Some(Caller::Token),
            _ => None,
        };
        if let Some(caller) = caller {
            request.extensions_mut().insert(caller);
        }
        next.run(request).await
    }

    #[tokio::test]
    async fn a_native_only_group_refuses_the_browser_credential() {
        let groups = [
            Group {
                audience: Audience::NativeOnly,
                routes: native_routes,
            },
            Group {
                audience: Audience::Browser,
                routes: shared_routes,
            },
        ];
        let app: Router = merge::<()>(&groups).layer(middleware::from_fn(admit_as));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = reqwest::Client::new();
        for (path, caller, want) in [
            ("/native", "browser", 403),
            ("/native", "none", 403),
            ("/native", "native", 200),
            ("/native", "token", 200),
            ("/shared", "browser", 200),
            ("/shared", "native", 200),
        ] {
            let status = client
                .get(format!("http://{addr}{path}"))
                .header("x-caller", caller)
                .send()
                .await
                .unwrap()
                .status();
            assert_eq!(status.as_u16(), want, "{path} as {caller}");
        }
    }
}
