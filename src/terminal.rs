//! One dedicated framed core connection per browser terminal. No payload logging.
use crate::{
    Shared,
    auth::{ApiError, error},
    core::Plain,
};
use anyhow::{Result, bail, ensure};
use axum::{
    extract::{
        Query, State, WebSocketUpgrade,
        ws::{CloseFrame, Message, WebSocket},
    },
    http::{HeaderMap, StatusCode},
    response::Response,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::VecDeque, time::Duration};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const HEADER: usize = 9;
const WINDOW: usize = 262144;
const MAX_PAYLOAD: usize = 16 << 20;
const MAX_CONTROL: usize = 1 << 20;
const MAX_INPUT: usize = 65536;
const DATA: u8 = 0;
const CREDIT: u8 = 1;
const CLOSE: u8 = 2;
const RESIZE: u8 = 3;
const FOCUS: u8 = 4;
const FACTS: u8 = 5;
const AUTOMATIC: u8 = 6;
const IO_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Deserialize, Default)]
#[serde(rename_all = "lowercase")]
enum TerminalTheme {
    Light,
    #[default]
    Dark,
}
impl TerminalTheme {
    fn display(&self) -> Value {
        match self {
            Self::Light => json!({"fg":"#4c4f69","bg":"#eff1f5","cursor":"#1e66f5","cell":[8,17]}),
            Self::Dark => json!({"fg":"#cdd6f4","bg":"#1e1e2e","cursor":"#89b4fa","cell":[8,17]}),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttachQuery {
    terminal: String,
    #[serde(default = "default_cols")]
    cols: u16,
    #[serde(default = "default_rows")]
    rows: u16,
    #[serde(default)]
    observe: bool,
    #[serde(default)]
    resumed: bool,
    #[serde(default)]
    theme: TerminalTheme,
}
fn default_cols() -> u16 {
    80
}
fn default_rows() -> u16 {
    24
}
fn valid_size(cols: u16, rows: u16) -> bool {
    cols > 0
        && rows > 0
        && cols <= 1000
        && rows <= 1000
        && u32::from(cols) * u32::from(rows) <= 250000
}

pub async fn upgrade(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<AttachQuery>,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    app.auth.require(&headers)?;
    app.auth.same_origin(&headers, true)?;
    if query.terminal.is_empty()
        || query.terminal.len() > 512
        || query.terminal.chars().any(char::is_control)
        || !valid_size(query.cols, query.rows)
    {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "Invalid terminal or dimensions",
        ));
    }
    Ok(ws.max_message_size(MAX_INPUT).max_frame_size(MAX_INPUT).write_buffer_size(0).max_write_buffer_size(WINDOW * 2)
        .on_upgrade(move |mut ws| async move {
            let outcome = session(&mut ws, &app, &headers, query).await;
            if outcome.is_err() {
                // Core errors may contain paths, host details or terminal text. Never reflect them.
                let _ = send_json(&mut ws, json!({"type":"error","message":"Terminal connection ended; reconnect to try again"})).await;
            }
            let _ = tokio::time::timeout(IO_TIMEOUT, ws.send(Message::Close(Some(CloseFrame {
                code: if outcome.is_ok() {1000} else {1011}, reason: "Terminal connection closed".into(),
            })))).await;
        }))
}

#[derive(Debug)]
struct Frame {
    channel: u32,
    kind: u8,
    payload: Vec<u8>,
}
fn encode(channel: u32, kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(HEADER + payload.len());
    bytes.extend(channel.to_le_bytes());
    bytes.push(kind);
    bytes.extend((payload.len() as u32).to_le_bytes());
    bytes.extend(payload);
    bytes
}
fn decode_header(h: &[u8; HEADER]) -> Result<(u32, u8, usize)> {
    let channel = u32::from_le_bytes(h[..4].try_into()?);
    let len = u32::from_le_bytes(h[5..].try_into()?) as usize;
    ensure!(len <= MAX_PAYLOAD, "oversized frame");
    // This connection only carries attach output and its single RPC reply.
    ensure!(
        len <= if channel != 0 && h[4] == DATA {
            WINDOW
        } else {
            MAX_CONTROL
        },
        "frame exceeds bridge limit"
    );
    ensure!(
        matches!(h[4], DATA | CLOSE | RESIZE | FACTS),
        "unexpected core frame"
    );
    if h[4] == RESIZE {
        ensure!(len == 4, "invalid resize");
    }
    Ok((channel, h[4], len))
}
async fn read_frame(r: &mut (impl AsyncRead + Unpin)) -> Result<Option<Frame>> {
    let mut h = [0; HEADER];
    // Distinguish clean EOF from a truncated header.
    if r.read(&mut h[..1]).await? == 0 {
        return Ok(None);
    }
    r.read_exact(&mut h[1..]).await?;
    let (channel, kind, len) = decode_header(&h)?;
    let mut payload = vec![0; len];
    r.read_exact(&mut payload).await?;
    Ok(Some(Frame {
        channel,
        kind,
        payload,
    }))
}
async fn write_frame(
    w: &mut (impl AsyncWrite + Unpin),
    ch: u32,
    kind: u8,
    payload: &[u8],
) -> Result<()> {
    tokio::time::timeout(IO_TIMEOUT, w.write_all(&encode(ch, kind, payload))).await??;
    Ok(())
}
async fn send_json(ws: &mut WebSocket, value: Value) -> Result<()> {
    tokio::time::timeout(IO_TIMEOUT, ws.send(Message::Text(value.to_string().into()))).await??;
    Ok(())
}

struct Attached {
    channel: u32,
    terminal: Value,
    automatic: bool,
    resize: bool,
    early: VecDeque<Frame>,
}
async fn attach(conn: &mut Plain, q: &AttachQuery) -> Result<Attached> {
    let request = json!({"jsonrpc":"2.0","id":2,"method":"terminal.attach","params":{
        "terminal":q.terminal,"cols":q.cols,"rows":q.rows,"observe":q.observe,"resumed":q.resumed,
        "resize_notifications":true,"history":2000,
        "display":q.theme.display()
    }});
    write_frame(conn.io.get_mut(), 0, DATA, request.to_string().as_bytes()).await?;
    let mut early = VecDeque::new();
    let mut bytes = 0;
    loop {
        let frame = read_frame(&mut conn.io)
            .await?
            .ok_or_else(|| anyhow::anyhow!("core EOF"))?;
        if frame.channel == 0 {
            ensure!(frame.kind == DATA, "invalid reply");
            let reply: Value = serde_json::from_slice(&frame.payload)?;
            ensure!(
                reply["id"] == 2 && reply.get("error").is_none(),
                "attach refused"
            );
            let r = &reply["result"];
            let channel = r["channel"]
                .as_u64()
                .filter(|n| *n > 0 && *n <= u32::MAX as u64)
                .ok_or_else(|| anyhow::anyhow!("invalid channel"))?
                as u32;
            ensure!(
                early.iter().all(|f: &Frame| f.channel == channel),
                "unexpected early channel"
            );
            let automatic = r["automatic_input"] == true
                && conn.hello["features"]
                    .as_array()
                    .is_some_and(|fs| fs.iter().any(|f| f == "automatic_input"));
            return Ok(Attached {
                channel,
                terminal: r["terminal"].clone(),
                automatic,
                resize: r["resize_notifications"] == true,
                early,
            });
        }
        bytes += frame.payload.len() + HEADER;
        ensure!(
            bytes <= WINDOW + 65536 && early.len() < 128,
            "too many early frames"
        );
        early.push_back(frame);
    }
}

/// The initial grant consumes the core's implicit WINDOW, not a second core grant.
#[derive(Default)]
struct Credit {
    started: bool,
    available: usize,
    outstanding: usize,
}
impl Credit {
    fn grant(&mut self, bytes: usize) -> Result<bool> {
        if !self.started {
            ensure!(bytes == WINDOW, "initial credit must equal window");
            self.started = true;
            self.available = WINDOW;
            return Ok(false);
        }
        ensure!(bytes > 0 && bytes <= self.outstanding, "invalid credit");
        self.outstanding -= bytes;
        self.available += bytes;
        ensure!(self.available <= WINDOW, "credit overflow");
        Ok(true)
    }
    fn sent(&mut self, bytes: usize) -> Result<()> {
        ensure!(bytes <= self.available, "core exceeded credit");
        self.available -= bytes;
        self.outstanding += bytes;
        Ok(())
    }
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Command {
    Input { data: String },
    Automatic { data: String },
    Resize { cols: u16, rows: u16 },
    Focus { focused: bool },
    Credit { bytes: usize },
}

async fn session(
    ws: &mut WebSocket,
    app: &Shared,
    headers: &HeaderMap,
    query: AttachQuery,
) -> Result<()> {
    // Includes hello and attach: logout cannot leave a stalled handshake alive indefinitely.
    let (conn, attached) = tokio::time::timeout(Duration::from_secs(8), async {
        let mut conn = Plain::connect(&app.core.path, true).await?;
        let attached = attach(&mut conn, &query).await?;
        Ok::<_, anyhow::Error>((conn, attached))
    })
    .await??;
    ensure!(app.auth.authorized(headers), "session revoked");
    let Attached {
        channel,
        terminal,
        automatic,
        resize,
        mut early,
    } = attached;
    send_json(ws, json!({"type":"attached","terminal":terminal,"automatic_input":automatic,"resize_notifications":resize,"observe":query.observe})).await?;
    // Keep BufReader intact: hello may have read ahead into the framed stream.
    let (mut reader, mut writer) = tokio::io::split(conn.io);
    let (tx, mut rx) = tokio::sync::mpsc::channel(2);
    let task = tokio::spawn(async move {
        loop {
            let result = read_frame(&mut reader).await;
            let end = !matches!(&result, Ok(Some(_)));
            if tx.send(result).await.is_err() || end {
                break;
            }
        }
    });
    // Aborting on every return prevents a detached reader retaining the core attachment.
    struct ReaderGuard(tokio::task::JoinHandle<()>);
    impl Drop for ReaderGuard {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let _guard = ReaderGuard(task);
    let mut credit = Credit::default();
    let mut pending = early.pop_front();
    let mut auth_tick = tokio::time::interval(Duration::from_secs(5));
    let initial_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let result: Result<()> = async {
        loop {
            // Also check during a continuous burst, not just when select is reached.
            ensure!(app.auth.authorized(headers), "session revoked");
            if let Some(frame) = pending.as_ref() {
                ensure!(frame.channel == channel, "unexpected channel");
                if frame.kind != DATA || (credit.started && frame.payload.len() <= credit.available) {
                    let frame = pending.take().unwrap();
                    match frame.kind {
                        DATA => {
                            credit.sent(frame.payload.len())?;
                            tokio::time::timeout(IO_TIMEOUT, ws.send(Message::Binary(frame.payload.into()))).await??;
                        }
                        RESIZE => {
                            let cols = u16::from_le_bytes(frame.payload[..2].try_into()?);
                            let rows = u16::from_le_bytes(frame.payload[2..].try_into()?);
                            ensure!(valid_size(cols, rows), "invalid core size");
                            send_json(ws, json!({"type":"resize","cols":cols,"rows":rows})).await?;
                        }
                        CLOSE => {
                            // The close payload is deliberately not exposed.
                            send_json(ws, json!({"type":"closed","reason":"Terminal stream ended"})).await?;
                            return Ok(());
                        }
                        FACTS => {},
                        _ => bail!("unexpected stream frame"),
                    }
                    pending = early.pop_front();
                    continue;
                }
            }
            tokio::select! {
                _ = auth_tick.tick() => ensure!(app.auth.authorized(headers), "session revoked"),
                _ = tokio::time::sleep_until(initial_deadline), if !credit.started => bail!("initial credit timeout"),
                frame = rx.recv(), if pending.is_none() => {
                    pending = Some(frame.ok_or_else(||anyhow::anyhow!("reader ended"))??.ok_or_else(||anyhow::anyhow!("core EOF"))?);
                }
                message = ws.recv() => {
                    match message {
                        Some(Ok(Message::Binary(data))) => {
                            ensure!(data.len() <= MAX_INPUT, "input too large");
                            if !query.observe && !data.is_empty() { write_frame(&mut writer, channel, DATA, &data).await?; }
                        }
                        Some(Ok(Message::Text(text))) => {
                            ensure!(text.len() <= MAX_INPUT, "command too large");
                            match serde_json::from_str::<Command>(&text)? {
                                Command::Credit {bytes} => if credit.grant(bytes)? {write_frame(&mut writer, channel, CREDIT, &(bytes as u32).to_le_bytes()).await?;},
                                Command::Input {data} => if !query.observe && !data.is_empty() {write_frame(&mut writer, channel, DATA, data.as_bytes()).await?;},
                                Command::Automatic {data} => if !query.observe && automatic && !data.is_empty() {write_frame(&mut writer, channel, AUTOMATIC, data.as_bytes()).await?;},
                                Command::Resize {cols, rows} => {
                                    ensure!(valid_size(cols, rows), "invalid dimensions");
                                    if !query.observe {let mut p = cols.to_le_bytes().to_vec(); p.extend(rows.to_le_bytes()); write_frame(&mut writer, channel, RESIZE, &p).await?;}
                                }
                                Command::Focus {focused} => if !query.observe {write_frame(&mut writer, channel, FOCUS, &[u8::from(focused)]).await?;},
                            }
                        }
                        Some(Ok(Message::Ping(data))) => {tokio::time::timeout(IO_TIMEOUT, ws.send(Message::Pong(data))).await??;}
                        Some(Ok(Message::Pong(_))) => {},
                        Some(Ok(Message::Close(_))) | None => return Ok(()),
                        Some(Err(_)) => bail!("websocket failed"),
                    }
                }
            }
        }
    }.await;
    let _ = write_frame(&mut writer, channel, CLOSE, &[]).await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn framing_roundtrip_and_limits() {
        let bytes = encode(42, DATA, b"hello");
        assert_eq!(&bytes[..9], &[42, 0, 0, 0, 0, 5, 0, 0, 0]);
        assert_eq!(
            decode_header(bytes[..9].try_into().unwrap()).unwrap(),
            (42, DATA, 5)
        );
        let mut h = [0; HEADER];
        h[5..].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(decode_header(&h).is_err());
        h[5..].copy_from_slice(&0u32.to_le_bytes());
        h[4] = RESIZE;
        assert!(decode_header(&h).is_err());
        h[4] = CLOSE;
        assert_eq!(decode_header(&h).unwrap(), (0, CLOSE, 0));
    }
    #[tokio::test]
    async fn eof_truncation_and_close() {
        let mut empty = &b""[..];
        assert!(read_frame(&mut empty).await.unwrap().is_none());
        let mut partial = &b"\x01\x00"[..];
        assert!(read_frame(&mut partial).await.is_err());
        let bytes = encode(7, CLOSE, b"{}");
        let frame = read_frame(&mut &bytes[..]).await.unwrap().unwrap();
        assert_eq!(
            (frame.channel, frame.kind, frame.payload),
            (7, CLOSE, b"{}".to_vec())
        );
        assert!(read_frame(&mut &bytes[..bytes.len() - 1]).await.is_err());
    }
    #[test]
    fn credit_does_not_double_initial_core_window_or_allow_forged_ack() {
        let mut c = Credit::default();
        assert!(c.grant(1).is_err());
        assert!(!c.grant(WINDOW).unwrap());
        assert!(c.grant(1).is_err());
        c.sent(100).unwrap();
        assert!(c.grant(101).is_err());
        assert!(c.grant(100).unwrap());
        assert_eq!(c.available, WINDOW);
        assert_eq!(c.outstanding, 0);
    }
    #[test]
    fn viewer_theme_is_validated_and_matches_terminal_colors() {
        let light: AttachQuery =
            serde_json::from_value(json!({"terminal":"t1","theme":"light"})).unwrap();
        let dark: AttachQuery =
            serde_json::from_value(json!({"terminal":"t1","theme":"dark"})).unwrap();
        assert_eq!(light.theme.display()["bg"], "#eff1f5");
        assert_eq!(dark.theme.display()["bg"], "#1e1e2e");
        assert!(
            serde_json::from_value::<AttachQuery>(json!({"terminal":"t1","theme":"invalid"}))
                .is_err()
        );
    }
    #[test]
    fn query_defaults_and_command_validation() {
        let q: AttachQuery = serde_json::from_value(json!({"terminal":"other/t1"})).unwrap();
        assert!(!q.observe && !q.resumed);
        let observer: AttachQuery =
            serde_json::from_value(json!({"terminal":"t1","observe":true})).unwrap();
        assert!(observer.observe);
        assert!(valid_size(q.cols, q.rows));
        assert!(!valid_size(0, 24));
        assert!(!valid_size(1000, 1000));
        assert!(
            serde_json::from_value::<Command>(json!({"type":"focus","focused":true,"extra":1}))
                .is_err()
        );
    }
}
