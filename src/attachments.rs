//! Bounded raw uploads through native core staging. No filesystem fallback or retries.
use crate::{
    Shared,
    auth::{ApiError, error},
};
use axum::{
    Extension, Json, Router,
    body::to_bytes,
    extract::{Query, Request, State},
    http::{StatusCode, header},
    routing::post,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::Semaphore;

const MAX_BYTES: usize = 8 << 20;
const TERMINAL_BUDGET: usize = 64 << 20;
const RUN_BUDGET: usize = 256 << 20;
const MAX_TERMINALS: usize = 128;

#[derive(Default)]
struct Budget {
    terminals: HashMap<String, usize>,
    total: usize,
}
impl Budget {
    // Never evict entries: reconnecting or alternating addresses must not reset quotas.
    // Charge even failed/ambiguous RPCs: core may already have staged the file.
    fn charge(&mut self, terminal: &str, bytes: usize) -> Result<(), ApiError> {
        let used = self.terminals.get(terminal).copied().unwrap_or(0);
        if bytes > TERMINAL_BUDGET - used
            || bytes > RUN_BUDGET - self.total
            || (!self.terminals.contains_key(terminal) && self.terminals.len() >= MAX_TERMINALS)
        {
            return Err(error(
                StatusCode::TOO_MANY_REQUESTS,
                "Image staging budget reached for this server run; remove staged files before restarting the server",
            ));
        }
        self.terminals.insert(terminal.to_owned(), used + bytes);
        self.total += bytes;
        Ok(())
    }
}
struct Uploads {
    slots: Semaphore,
    budget: Mutex<Budget>,
}
impl Default for Uploads {
    fn default() -> Self {
        Self {
            slots: Semaphore::new(2),
            budget: Mutex::new(Budget::default()),
        }
    }
}
pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/api/attachments", post(handle))
        .layer(Extension(Arc::new(Uploads::default())))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UploadQuery {
    terminal: String,
}

async fn handle(
    State(app): State<Shared>,
    Extension(uploads): Extension<Arc<Uploads>>,
    request: Request,
) -> Result<Json<Value>, ApiError> {
    // Request (not Bytes/Json) keeps authorization ahead of all body collection.
    let headers = request.headers();
    app.auth.require(headers)?;
    app.auth.same_origin(headers, false)?;
    let Query(query) = Query::<UploadQuery>::try_from_uri(request.uri()).map_err(|_| {
        error(
            StatusCode::BAD_REQUEST,
            "Supply only a selected terminal address",
        )
    })?;
    if query.terminal.is_empty()
        || query.terminal.len() > 512
        || query.terminal.chars().any(char::is_control)
    {
        return Err(error(StatusCode::BAD_REQUEST, "Invalid terminal address"));
    }
    if headers
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        != Some("application/octet-stream")
    {
        return Err(error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Send raw image bytes as application/octet-stream",
        ));
    }
    if headers.contains_key(header::CONTENT_ENCODING) {
        return Err(error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Encoded request bodies are not supported",
        ));
    }
    if headers
        .get(header::CONTENT_LENGTH)
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .is_some_and(|n| n > MAX_BYTES as u64)
    {
        return Err(error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "Images must be at most 8 MiB",
        ));
    }
    let _slot = uploads.slots.try_acquire().map_err(|_| {
        error(
            StatusCode::TOO_MANY_REQUESTS,
            "Two image uploads are already in progress; wait before trying again",
        )
    })?;
    let bytes = tokio::time::timeout(
        Duration::from_secs(30),
        to_bytes(request.into_body(), MAX_BYTES),
    )
    .await
    .map_err(|_| error(StatusCode::REQUEST_TIMEOUT, "Image body upload timed out"))?
    .map_err(|e| {
        use std::error::Error;
        if e.source()
            .is_some_and(|e| e.is::<http_body_util::LengthLimitError>())
        {
            error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "Images must be at most 8 MiB",
            )
        } else {
            error(StatusCode::BAD_REQUEST, "Could not read image body")
        }
    })?;
    let ext = image_extension(&bytes).ok_or_else(|| {
        error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Expected a PNG, JPEG, GIF or WebP image with a valid header",
        )
    })?;
    uploads
        .budget
        .lock()
        .unwrap()
        .charge(&query.terminal, bytes.len())?;
    let result = app.core.call("terminal.put", json!({"terminal":query.terminal,"name":format!("{}.{}", uuid::Uuid::new_v4(), ext),"data":STANDARD.encode(&bytes)})).await
        // Do not echo native errors: they can contain arbitrary peer data or request contents.
        .map_err(|_| error(StatusCode::BAD_GATEWAY, "Core could not confirm image staging (terminal.put may be unsupported or unavailable). The upload may already exist; no automatic retry was made"))?;
    let path = result["path"].as_str().filter(|p| valid_path(p));
    if result["bytes"].as_u64() != Some(bytes.len() as u64) || path.is_none() {
        return Err(error(
            StatusCode::BAD_GATEWAY,
            "Core returned an invalid staging response. The upload may already exist; no automatic retry was made",
        ));
    }
    Ok(Json(
        json!({"path":path.unwrap(),"bytes":bytes.len(),"terminal":query.terminal}),
    ))
}
fn valid_path(path: &str) -> bool {
    path.starts_with('/')
        && path.len() <= 4096
        && !path.chars().any(char::is_control)
        && !path.split('/').any(|part| matches!(part, "." | ".."))
        && !path.ends_with('/')
}

// Basic container/header checks, not a full image decoder. Image bytes are never executed.
fn image_extension(b: &[u8]) -> Option<&'static str> {
    if b.len() >= 33
        && b.starts_with(b"\x89PNG\r\n\x1a\n")
        && b[8..16] == *b"\0\0\0\rIHDR"
        && b[16..20] != [0; 4]
        && b[20..24] != [0; 4]
        && b[26] == 0
        && b[27] == 0
        && b[28] <= 1
    {
        return Some("png");
    }
    if b.len() >= 14
        && (b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a"))
        && b[6..8] != [0; 2]
        && b[8..10] != [0; 2]
    {
        return Some("gif");
    }
    if b.len() >= 20
        && b.starts_with(b"RIFF")
        && &b[8..12] == b"WEBP"
        && u32::from_le_bytes(b[4..8].try_into().ok()?) as usize + 8 == b.len()
    {
        let size = u32::from_le_bytes(b[16..20].try_into().ok()?) as usize;
        if size <= b.len() - 20
            && match &b[12..16] {
                b"VP8 " => size >= 10 && b[23..26] == [0x9d, 0x01, 0x2a],
                b"VP8L" => size >= 5 && b[20] == 0x2f,
                b"VP8X" => size == 10 && b[21..24] == [0; 3],
                _ => false,
            }
        {
            return Some("webp");
        }
    }
    if b.len() >= 4 && b.starts_with(&[0xff, 0xd8]) && b.ends_with(&[0xff, 0xd9]) {
        let mut i = 2;
        while i + 4 <= b.len() {
            if b[i] != 0xff {
                break;
            }
            let marker = b[i + 1];
            if marker == 0xff {
                i += 1;
                continue;
            }
            let size = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
            if size < 2 || i + 2 + size > b.len() {
                break;
            }
            if matches!(marker, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf) {
                if size >= 8 && b[i + 5..i + 7] != [0; 2] && b[i + 7..i + 9] != [0; 2] {
                    return Some("jpg");
                }
                break;
            }
            if matches!(marker, 0xda | 0xd9) {
                break;
            }
            i += 2 + size;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{App, auth::Auth, core::Core};
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tower::ServiceExt;

    fn image() -> Vec<u8> {
        b"GIF89a\x01\0\x01\0\0\0\0;".to_vec()
    }
    fn app(core: Arc<Core>) -> Shared {
        Arc::new(App {
            updates: crate::update::Updater::development(),
            core,
            auth: Auth::new("a".repeat(64), None),
        })
    }
    fn request(_body: Vec<u8>) -> axum::http::request::Builder {
        Request::post("/api/attachments?terminal=remote%2Ft1")
            .header("authorization", format!("Bearer {}", "a".repeat(64)))
            .header("host", "localhost:8001")
            .header("origin", "http://localhost:8001")
            .header("content-type", "application/octet-stream")
    }
    #[test]
    fn signatures_and_bounded_budgets() {
        assert_eq!(image_extension(&image()), Some("gif"));
        for b in [
            b"not an image".as_slice(),
            b"GIF89a",
            b"\xff\xd8\xff\xd9",
            b"\x89PNG\r\n\x1a\n",
            b"RIFF\0\0\0\0WEBP",
        ] {
            assert_eq!(image_extension(b), None);
        }
        let mut png = vec![0; 33];
        png[..16].copy_from_slice(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR");
        png[19] = 1;
        png[23] = 1;
        png[24] = 8;
        png[25] = 2;
        assert_eq!(image_extension(&png), Some("png"));
        let jpeg = [0xff, 0xd8, 0xff, 0xc0, 0, 8, 8, 0, 1, 0, 1, 0, 0xff, 0xd9];
        assert_eq!(image_extension(&jpeg), Some("jpg"));
        let mut webp = b"RIFF\x12\0\0\0WEBPVP8L\x05\0\0\0\x2f\0\0\0\0\0".to_vec();
        assert_eq!(image_extension(&webp), Some("webp"));
        webp[20] = 0;
        assert_eq!(image_extension(&webp), None);
        let mut budget = Budget::default();
        for _ in 0..8 {
            budget.charge("t1", MAX_BYTES).unwrap();
        }
        assert_eq!(
            budget.charge("t1", 1).unwrap_err().0,
            StatusCode::TOO_MANY_REQUESTS
        );
        for t in ["t2", "t3", "t4"] {
            budget.charge(t, TERMINAL_BUDGET).unwrap();
        }
        assert!(budget.charge("t5", 1).is_err());
        let mut budget = Budget::default();
        for i in 0..MAX_TERMINALS {
            budget.charge(&i.to_string(), 1).unwrap();
        }
        assert!(budget.charge("new", 1).is_err());
        assert_eq!(budget.terminals.len(), MAX_TERMINALS);
        for p in ["", "relative.png", "/tmp/../x", "/tmp/x\n", "/tmp/"] {
            assert!(!valid_path(p));
        }
    }
    #[tokio::test]
    async fn auth_origin_body_limits_and_concurrency() {
        let state = app(Core::new("/nonexistent".into()));
        let router = crate::router(state.clone());
        let response = router
            .clone()
            .oneshot(
                Request::post("/api/attachments")
                    .body(Body::from(vec![0; MAX_BYTES + 1]))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 401);
        let mut foreign = request(image()).body(Body::from(image())).unwrap();
        foreign
            .headers_mut()
            .insert(header::ORIGIN, "http://evil.example".parse().unwrap());
        let response = router.clone().oneshot(foreign).await.unwrap();
        assert_eq!(response.status(), 403);
        let response = router
            .clone()
            .oneshot(
                request(image())
                    .body(Body::from(vec![0; MAX_BYTES + 1]))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 413);
        let response = router
            .clone()
            .oneshot(request(image()).body(Body::from(vec![0; 2 << 20])).unwrap())
            .await
            .unwrap();
        // The raw route bypasses the default 1 MiB extractor limit, then sniffs bytes.
        assert_eq!(response.status(), 415);
        let response = router
            .clone()
            .oneshot(
                Request::post("/api/rpc")
                    .header("content-type", "application/json")
                    .body(Body::from(vec![b' '; (1 << 20) + 1]))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 413);
        let mut maximum_image = image();
        maximum_image.resize(MAX_BYTES, 0);
        let response = router
            .clone()
            .oneshot(request(image()).body(Body::from(maximum_image)).unwrap())
            .await
            .unwrap();
        // Exactly 8 MiB reaches native staging (the intentionally unavailable core).
        assert_eq!(response.status(), 502);
        let response = router
            .oneshot(request(image()).body(Body::from(image())).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), 502);
        let uploads = Arc::new(Uploads::default());
        let _one = uploads.slots.try_acquire().unwrap();
        let _two = uploads.slots.try_acquire().unwrap();
        let response = handle(
            State(state),
            Extension(uploads.clone()),
            request(image()).body(Body::from(image())).unwrap(),
        )
        .await
        .unwrap_err();
        assert_eq!(response.0, StatusCode::TOO_MANY_REQUESTS);
    }
    #[tokio::test]
    async fn native_routing_success_and_sanitized_failures() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("core.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let mock = tokio::spawn(async move {
            let (io, _) = listener.accept().await.unwrap();
            let mut io = BufReader::new(io);
            for index in 0..4 {
                let mut line = String::new();
                io.read_line(&mut line).await.unwrap();
                let req: Value = serde_json::from_str(&line).unwrap();
                let payload = if index == 0 {
                    assert_eq!(req["method"], "hello");
                    json!({"result":{"generation":1}})
                } else {
                    assert_eq!(req["method"], "terminal.put");
                    assert_eq!(req["params"]["terminal"], "remote/t1");
                    assert_eq!(
                        STANDARD
                            .decode(req["params"]["data"].as_str().unwrap())
                            .unwrap(),
                        image()
                    );
                    let name = req["params"]["name"].as_str().unwrap();
                    assert!(name.ends_with(".gif"));
                    uuid::Uuid::parse_str(name.strip_suffix(".gif").unwrap()).unwrap();
                    match index {
                        1 => {
                            json!({"result":{"path":"/state/files/t1/image.gif","bytes":image().len()}})
                        }
                        2 => json!({"result":{"path":"/state/../bad","bytes":image().len()}}),
                        _ => json!({"error":{"message":"unsupported terminal.put SECRET_BASE64"}}),
                    }
                };
                let mut payload = payload;
                payload["id"] = req["id"].clone();
                io.get_mut()
                    .write_all(format!("{payload}\n").as_bytes())
                    .await
                    .unwrap();
            }
        });
        let router = crate::router(app(Core::new(path)));
        for index in 0..3 {
            let response = router
                .clone()
                .oneshot(request(image()).body(Body::from(image())).unwrap())
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                if index == 0 {
                    StatusCode::OK
                } else {
                    StatusCode::BAD_GATEWAY
                }
            );
            let body = response.into_body().collect().await.unwrap().to_bytes();
            let text = String::from_utf8(body.to_vec()).unwrap();
            assert!(!text.contains("SECRET_BASE64"));
            if index == 0 {
                let result: Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(result["terminal"], "remote/t1");
                assert_eq!(result["bytes"], image().len());
            } else {
                assert!(text.contains("may already exist"));
            }
        }
        mock.await.unwrap();
    }
}
