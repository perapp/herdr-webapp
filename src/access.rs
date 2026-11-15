//! Local-server access settings. No access token is ever returned over HTTP.
use crate::{
    Shared,
    auth::{ApiError, error},
    notifications::Notifications,
};
use axum::{
    Extension, Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

struct AccessControl {
    notifications: Option<Arc<Notifications>>,
}

/// Mount once in the LAN router, never the relay router.
/// The gate serializes the entire rotation + push-owner pruning operation.
pub fn router(notifications: Option<Arc<Notifications>>) -> Router<Shared> {
    Router::new()
        .route("/api/access", get(status))
        .route("/api/access/rotate", post(rotate))
        .layer(Extension(Arc::new(AccessControl { notifications })))
}

pub async fn status(
    State(app): State<Shared>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    app.auth.require(&headers)?;
    Ok(Json(
        json!({"supported":true,"canRotate":true,"scope":"server",
        "tokenLifetime":"untilRotated","sessionLifetime":"untilRevoked"}),
    ))
}

#[derive(Deserialize)]
#[serde(try_from = "Value")]
pub struct RotateRequest {}

impl TryFrom<Value> for RotateRequest {
    type Error = &'static str;
    fn try_from(value: Value) -> Result<Self, Self::Error> {
        if value.as_object().is_some_and(|object| object.is_empty()) {
            Ok(Self {})
        } else {
            Err("Expected an empty JSON object")
        }
    }
}

async fn rotate(
    State(app): State<Shared>,
    Extension(control): Extension<Arc<AccessControl>>,
    headers: HeaderMap,
    Json(_body): Json<RotateRequest>,
) -> Result<Json<Value>, ApiError> {
    // Browser-only mutation: always require exact Origin and an existing cookie.
    app.auth.same_origin(&headers, true)?;
    // Shared with LAN notification enrollment: a queued request must recheck
    // Auth under this gate before it can authorize/register any push owner.
    let _guard = app.auth.authorization_write().await;
    // Recheck after waiting: another profile may have rotated while we queued.
    let owner = app.auth.rotate(&headers)?;
    if let Some(notifications) = &control.notifications {
        notifications.keep_owner(&owner).await.map_err(|_| {
            error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Access token rotated; notification storage unavailable",
            )
        })?;
    }
    // No Set-Cookie: preserve the initiating identity and active WebSockets.
    Ok(Json(json!({"rotated":true})))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

    fn app() -> Shared {
        Arc::new(crate::App {
            core: crate::core::Core::new("/nonexistent".into()),
            updates: crate::update::Updater::development(),
            auth: crate::auth::Auth::new("a".repeat(64), None),
        })
    }

    #[tokio::test]
    async fn access_status_is_authenticated_and_never_contains_token() {
        let app = app();
        let router = router(None).with_state(app);
        let denied = router
            .clone()
            .oneshot(Request::get("/api/access").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
        let response = router
            .oneshot(
                Request::get("/api/access")
                    .header("authorization", format!("Bearer {}", "a".repeat(64)))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            value,
            json!({"supported":true,"canRotate":true,"scope":"server",
            "tokenLifetime":"untilRotated","sessionLifetime":"untilRevoked"})
        );
        assert!(!String::from_utf8_lossy(&body).contains(&"a".repeat(64)));
    }

    #[tokio::test]
    async fn rotation_http_boundary_preserves_cookie_and_rejects_untrusted_requests() {
        let app = app();
        let current = app.auth.login(&"a".repeat(64)).unwrap();
        let other = app.auth.login(&"a".repeat(64)).unwrap();
        let router = router(None).with_state(app.clone());
        for (origin, cookie, body, status) in [
            ("http://evil", current.as_str(), "{}", StatusCode::FORBIDDEN),
            ("", current.as_str(), "{}", StatusCode::FORBIDDEN),
            ("http://server:8001", "", "{}", StatusCode::UNAUTHORIZED),
            (
                "http://server:8001",
                current.as_str(),
                "{\"token\":\"x\"}",
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            (
                "http://server:8001",
                current.as_str(),
                "[]",
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
        ] {
            let response = router
                .clone()
                .oneshot(
                    Request::post("/api/access/rotate")
                        .header("content-type", "application/json")
                        .header("host", "server:8001")
                        .header("origin", origin)
                        .header("cookie", cookie.split(';').next().unwrap())
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), status);
        }
        let response = router
            .oneshot(
                Request::post("/api/access/rotate")
                    .header("content-type", "application/json")
                    .header("host", "server:8001")
                    .header("origin", "http://server:8001")
                    .header("cookie", current.split(';').next().unwrap())
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(!response.headers().contains_key("set-cookie"));
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap(),
            json!({"rotated":true})
        );
        let mut headers = HeaderMap::new();
        headers.insert(
            "cookie",
            current.split(';').next().unwrap().parse().unwrap(),
        );
        assert!(app.auth.authorized(&headers));
        headers.insert("cookie", other.split(';').next().unwrap().parse().unwrap());
        assert!(!app.auth.authorized(&headers));
    }

    #[test]
    fn rotation_body_is_strict_empty_object() {
        assert!(serde_json::from_str::<RotateRequest>("{}").is_ok());
        for invalid in ["null", "[]", "{\"token\":\"x\"}", "{\"path\":\"/tmp/x\"}"] {
            assert!(serde_json::from_str::<RotateRequest>(invalid).is_err());
        }
    }
}
