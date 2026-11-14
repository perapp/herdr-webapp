//! Outbound-only authenticated relay endpoint. Signaling never carries Core operations.
use crate::{
    core::{Core, Plain},
    peer_service::{PeerMessage, PeerService},
    relay_crypto::{self, Identity},
    relay_pairings::Pairings,
    relay_wire::{self, FrameDecoder},
};
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::IsTerminal,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{Mutex, Semaphore, mpsc},
    task::JoinSet,
};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};
use url::Url;
use webrtc::{
    api::APIBuilder,
    data_channel::{RTCDataChannel, data_channel_message::DataChannelMessage},
    ice_transport::ice_server::RTCIceServer,
    peer_connection::{
        configuration::RTCConfiguration, peer_connection_state::RTCPeerConnectionState,
        policy::ice_transport_policy::RTCIceTransportPolicy,
        sdp::session_description::RTCSessionDescription,
    },
};

// Browser budget: 32 terminals, two uploads and control, with some headroom.
const MAX_CONCURRENT_CHANNELS: usize = 40;
const MESSAGE_SEND_TIMEOUT: Duration = Duration::from_secs(2);

pub struct ConnectorConfig {
    pub relay_url: String,
    pub core_socket: PathBuf,
    pub state_dir: PathBuf,
    pub ready_file: Option<PathBuf>,
    pub allow_http_loopback: bool,
    pub force_turn: bool,
    pub reset_pairings: bool,
    pub no_dashboard: bool,
}
fn origin_url(input: &str, allow_loopback: bool) -> Result<crate::relay_channel::RelayEndpoint> {
    crate::relay_channel::RelayEndpoint::parse(input, allow_loopback)
}
fn transcript(origin: &str, machine: &str, session: &str, seq: u64, body: &str) -> String {
    format!("herdr-signal-v1\n{origin}\n{machine}\n{session}\n{seq}\n{body}")
}
fn verify_envelope(
    value: &Value,
    key: &str,
    origin: &str,
    machine: &str,
    session: &str,
    previous: &mut u64,
) -> Result<Value> {
    ensure!(value["kind"] == "signed", "expected signed signaling");
    let seq = value["seq"]
        .as_u64()
        .context("missing signaling sequence")?;
    ensure!(
        seq > *previous && seq <= 9_007_199_254_740_991,
        "signaling replay or invalid sequence"
    );
    let body = value["body"].as_str().context("missing signed body")?;
    ensure!(body.len() <= 512 * 1024, "signaling body too large");
    relay_crypto::verify(
        key,
        transcript(origin, machine, session, seq, body).as_bytes(),
        value["signature"].as_str().context("missing signature")?,
    )?;
    let body = serde_json::from_str(body)?;
    *previous = seq;
    Ok(body)
}
struct Endpoint {
    identity: Arc<Identity>,
    origin: String,
    machine: String,
    pairings: Arc<Mutex<Pairings>>,
    core: Arc<Core>,
    force_turn: bool,
    outgoing: mpsc::Sender<Value>,
}
impl Endpoint {
    async fn signed(&self, session: &str, seq: u64, body: Value) -> Result<()> {
        let body = serde_json::to_string(&body)?;
        let signature = self
            .identity
            .sign(transcript(&self.origin, &self.machine, session, seq, &body).as_bytes());
        self.send(json!({"type":"signal","sessionId":session,"payload":json!({"kind":"signed","seq":seq,"body":body,"signature":signature}).to_string()})).await
    }
    async fn send(&self, value: Value) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(2), self.outgoing.send(value)).await??;
        Ok(())
    }
}
async fn receive(rx: &mut mpsc::Receiver<Value>) -> Result<Value> {
    tokio::time::timeout(Duration::from_secs(30), rx.recv())
        .await?
        .context("session ended")
}
fn payload(value: &Value) -> Result<Value> {
    ensure!(value["type"] == "signal", "unexpected session message");
    let text = value["payload"]
        .as_str()
        .context("missing signal payload")?;
    ensure!(text.len() <= 600 * 1024, "signal too large");
    Ok(serde_json::from_str(text)?)
}
struct ReservationGuard {
    pairings: Arc<Mutex<Pairings>>,
    session: String,
}
impl Drop for ReservationGuard {
    fn drop(&mut self) {
        let pairings = self.pairings.clone();
        let session = self.session.clone();
        tokio::spawn(async move {
            pairings.lock().await.release(&session);
        });
    }
}
struct ConnectionGuard(Arc<webrtc::peer_connection::RTCPeerConnection>);
impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        let pc = self.0.clone();
        tokio::spawn(async move {
            let _ = pc.close().await;
        });
    }
}
async fn session(endpoint: Arc<Endpoint>, id: String, mut incoming: mpsc::Receiver<Value>) {
    // RAII also releases reservations if bounded supervisor shutdown aborts a worker.
    let _reservation = ReservationGuard {
        pairings: endpoint.pairings.clone(),
        session: id.clone(),
    };
    let _ = session_inner(endpoint.clone(), &id, &mut incoming).await;
    endpoint.pairings.lock().await.release(&id);
}
async fn session_inner(
    e: Arc<Endpoint>,
    id: &str,
    incoming: &mut mpsc::Receiver<Value>,
) -> Result<()> {
    let auth = payload(&receive(incoming).await?)?;
    ensure!(auth["kind"] == "auth", "expected authentication");
    let key = auth["publicKey"]
        .as_str()
        .context("missing browser key")?
        .to_owned();
    relay_crypto::machine_id(&key)?;
    let nonce = auth["nonce"].as_str().context("missing nonce")?;
    ensure!(
        (16..=256).contains(&nonce.len()) && !nonce.contains('\n'),
        "invalid nonce"
    );
    let proof = auth["proof"].as_str().context("missing proof")?;
    let pairing = match auth["mode"].as_str() {
        Some("pair") => true,
        Some("resume") => false,
        _ => bail!("invalid auth mode"),
    };
    let message = format!(
        "herdr-auth-v1\n{}\n{}\n{id}\n{key}\n{nonce}",
        e.origin, e.machine
    );
    {
        let mut pairings = e.pairings.lock().await;
        if pairing {
            pairings.reserve(id, message.as_bytes(), proof)?;
        } else {
            ensure!(pairings.known(&key), "unknown browser key");
            relay_crypto::verify(&key, message.as_bytes(), proof)?;
        }
    }
    e.signed(
        id,
        1,
        json!({"kind":"accepted","version":env!("CARGO_PKG_VERSION"),"protocolVersion":1}),
    )
    .await?;
    e.send(json!({"type":"authorize","sessionId":id})).await?;
    let mut ice = None;
    let mut offer = None;
    let mut seq = 0;
    while ice.is_none() || offer.is_none() {
        let value = receive(incoming).await?;
        if value["type"] == "ice" {
            ensure!(ice.is_none(), "duplicate ICE authorization");
            let entries = value["iceServers"]
                .as_array()
                .context("missing ICE servers")?;
            ensure!(entries.len() <= 8, "too many ICE servers");
            let mut servers = Vec::new();
            for entry in entries {
                let urls: Vec<String> = serde_json::from_value(entry["urls"].clone())?;
                ensure!(
                    urls.len() <= 8 && urls.iter().all(|u| u.len() <= 2048),
                    "invalid ICE URLs"
                );
                servers.push(RTCIceServer {
                    urls,
                    username: entry["username"].as_str().unwrap_or("").into(),
                    credential: entry["credential"].as_str().unwrap_or("").into(),
                });
            }
            ice = Some(servers);
        } else {
            ensure!(offer.is_none(), "duplicate offer");
            let body =
                verify_envelope(&payload(&value)?, &key, &e.origin, &e.machine, id, &mut seq)?;
            ensure!(body["kind"] == "offer", "expected offer");
            offer = Some(RTCSessionDescription::offer(
                body["sdp"]
                    .as_str()
                    .context("missing offer SDP")?
                    .to_owned(),
            )?);
        }
    }
    let api = APIBuilder::new().build();
    let pc = Arc::new(
        api.new_peer_connection(RTCConfiguration {
            ice_servers: ice.unwrap(),
            ice_transport_policy: if e.force_turn {
                RTCIceTransportPolicy::Relay
            } else {
                RTCIceTransportPolicy::All
            },
            ..Default::default()
        })
        .await?,
    );
    let _connection = ConnectionGuard(pc.clone());
    let (channels_tx, mut channels_rx) = mpsc::channel(MAX_CONCURRENT_CHANNELS);
    pc.on_data_channel(Box::new(move |channel| {
        let tx = channels_tx.clone();
        Box::pin(async move {
            if let Err(error) = tx.try_send(channel) {
                let _ = error.into_inner().close().await;
            }
        })
    }));
    let (failed_tx, mut failed_rx) = mpsc::channel(1);
    pc.on_peer_connection_state_change(Box::new(move |state| {
        let tx = failed_tx.clone();
        Box::pin(async move {
            if matches!(
                state,
                RTCPeerConnectionState::Failed
                    | RTCPeerConnectionState::Closed
                    | RTCPeerConnectionState::Disconnected
            ) {
                let _ = tx.try_send(());
            }
        })
    }));
    let mut handlers = JoinSet::new();
    let result: Result<()> = async {
        pc.set_remote_description(offer.unwrap()).await?;
        let answer = pc.create_answer(None).await?;
        let mut gathered = pc.gathering_complete_promise().await;
        pc.set_local_description(answer).await?;
        tokio::select! {
            gathered = tokio::time::timeout(Duration::from_secs(30), gathered.recv()) => { gathered?; },
            _ = incoming.recv() => bail!("session ended during ICE gathering"),
            _ = failed_rx.recv() => bail!("peer connection failed during ICE gathering"),
        }
        let local = pc.local_description().await.context("missing local SDP")?;
        e.signed(id, 2, json!({"kind":"answer","sdp":local.sdp})).await?;
        let ready = Arc::new(AtomicBool::new(false));
        let control = Arc::new(AtomicBool::new(false));
        let service = PeerService::new(e.core.clone());
        let channel_budget = Arc::new(Semaphore::new(MAX_CONCURRENT_CHANNELS));
        let hello_deadline = tokio::time::sleep(Duration::from_secs(30));
        tokio::pin!(hello_deadline);
        loop { tokio::select! {
            _ = &mut hello_deadline, if !ready.load(Ordering::SeqCst) => bail!("control hello timed out"),
            _ = failed_rx.recv() => break,
            value = incoming.recv() => { if value.is_none() { break; } bail!("unexpected post-answer signaling"); },
            Some(channel) = channels_rx.recv() => {
                let Ok(permit) = channel_budget.clone().try_acquire_owned() else {
                    let _ = channel.close().await;
                    continue;
                };
                let label = channel.label();
                let is_control = label == "herdr-control-v1";
                if is_control {
                    if control.swap(true, Ordering::SeqCst) { let _ = channel.close().await; bail!("duplicate control channel"); }
                } else if !ready.load(Ordering::SeqCst) { let _ = channel.close().await; continue; }
                let e = e.clone(); let id = id.to_owned(); let key = key.clone(); let ready = ready.clone(); let service = service.clone();
                handlers.spawn(async move {
                    // Release admission on every exit, including abort and panic.
                    let _permit = permit;
                    let outcome = serve_data_channel(channel.clone(), service, e, id, key, pairing, ready).await;
                    let _ = channel.close().await;
                    (is_control, outcome.is_ok())
                });
            },
            Some(done) = handlers.join_next(), if !handlers.is_empty() => {
                if !matches!(done, Ok((false, true))) { break; }
            },
        } }
        Ok(())
    }.await;
    handlers.abort_all();
    while handlers.join_next().await.is_some() {}
    let _ = pc.close().await;
    result
}
async fn serve_data_channel(
    dc: Arc<RTCDataChannel>,
    service: PeerService,
    e: Arc<Endpoint>,
    session: String,
    key: String,
    pairing: bool,
    ready: Arc<AtomicBool>,
) -> Result<()> {
    let label = dc.label().to_owned();
    let limit = if label == "herdr-control-v1" {
        2 * 1024 * 1024
    } else {
        64 * 1024
    };
    ensure!(
        label == "herdr-control-v1"
            || label.starts_with("herdr-terminal-v1:")
            || label.starts_with("herdr-upload-v1:"),
        "unknown channel"
    );
    ensure!(
        dc.ordered() && dc.max_retransmits().is_none() && dc.max_packet_lifetime().is_none(),
        "channel must be reliable and ordered"
    );
    let (packets, mut packet_rx) = mpsc::channel::<Vec<u8>>(64);
    let closed = Arc::new(AtomicBool::new(false));
    let bad = closed.clone();
    let close_dc = Arc::downgrade(&dc);
    dc.on_message(Box::new(move |message: DataChannelMessage| {
        let tx = packets.clone();
        let bad = bad.clone();
        let close_dc = close_dc.clone();
        Box::pin(async move {
            if message.is_string
                || message.data.len() > 16384
                || tx.try_send(message.data.to_vec()).is_err()
            {
                bad.store(true, Ordering::SeqCst);
                if let Some(dc) = close_dc.upgrade() {
                    let _ = dc.close().await;
                }
            }
        })
    }));
    let closed_flag = closed.clone();
    dc.on_close(Box::new(move || {
        let flag = closed_flag.clone();
        Box::pin(async move {
            flag.store(true, Ordering::SeqCst);
        })
    }));
    let (application_tx, application_rx) = mpsc::channel(32);
    let (out_tx, mut out_rx) = mpsc::channel(32);
    let mut decoder = FrameDecoder::new(limit);
    let mut next = 1u32;
    if label == "herdr-control-v1" {
        let hello = tokio::time::timeout(Duration::from_secs(15), async {
            let mut alive = tokio::time::interval(Duration::from_millis(100));
            loop {
                let packet = tokio::select! {
                    _ = alive.tick() => {
                        ensure!(!closed.load(Ordering::SeqCst), "channel closed before hello");
                        decoder.check_expired()?;
                        continue;
                    },
                    packet = packet_rx.recv() => packet.context("channel closed before hello")?,
                };
                if let Some((kind, bytes)) = decoder.push(&packet)? {
                    ensure!(kind == 0, "hello must be text");
                    let hello: Value = serde_json::from_slice(&bytes)?;
                    ensure!(
                        hello["type"] == "hello"
                            && hello["protocolVersion"] == 1
                            && hello["sessionId"] == session,
                        "invalid control hello"
                    );
                    return Ok::<(), anyhow::Error>(());
                }
            }
        })
        .await?;
        hello?;
        e.pairings.lock().await.complete(&session, &key, pairing)?;
        // Persistence/authentication precede anything the browser can observe.
        // A failed ready send exits this control handler and tears down the session.
        ready.store(true, Ordering::SeqCst);
        send_message(
            &dc,
            &mut next,
            PeerMessage::Text(
                json!({"type":"ready","protocolVersion":1,"version":env!("CARGO_PKG_VERSION")})
                    .to_string(),
            ),
        )
        .await?;
    }
    ensure!(ready.load(Ordering::SeqCst), "control not ready");
    let service_future = service.serve_channel(&label, application_rx, out_tx);
    tokio::pin!(service_future);
    let mut alive = tokio::time::interval(Duration::from_millis(100));
    let mut output_open = true;
    loop {
        tokio::select! {
            result = &mut service_future => {
                // One-shot services enqueue their final result before returning.
                // Completion must not race that queued response or a clean sender EOF.
                while let Ok(message) = out_rx.try_recv() {
                    send_message(&dc, &mut next, message).await?;
                }
                return result;
            },
            _ = alive.tick() => {
                ensure!(!closed.load(Ordering::SeqCst), "channel closed");
                decoder.check_expired()?;
            },
            packet = packet_rx.recv() => {
                let packet = packet.context("channel closed")?;
                if let Some((kind, bytes)) = decoder.push(&packet)? {
                    let message = if kind == 0 { PeerMessage::Text(String::from_utf8(bytes)?) } else { PeerMessage::Binary(bytes) };
                    tokio::time::timeout(Duration::from_secs(2), application_tx.send(message)).await??;
                }
            },
            message = out_rx.recv(), if output_open => {
                if let Some(message) = message { send_message(&dc, &mut next, message).await?; }
                else { output_open = false; }
            },
        }
    }
}
async fn send_message(dc: &RTCDataChannel, next: &mut u32, message: PeerMessage) -> Result<()> {
    let (kind, bytes) = match message {
        PeerMessage::Text(text) => (0, text.into_bytes()),
        PeerMessage::Binary(bytes) => (1, bytes),
    };
    let id = *next;
    *next = next
        .checked_add(1)
        .context("channel message IDs exhausted")?;
    send_packets(relay_wire::encode(kind, id, &bytes)?, |packet| async move {
        while dc.buffered_amount().await + packet.len() > 512 * 1024 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        dc.send(&packet.into()).await?;
        Ok(())
    })
    .await
}

async fn send_packets<F, Fut>(packets: Vec<Vec<u8>>, mut send: F) -> Result<()>
where
    F: FnMut(Vec<u8>) -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    // One deadline covers the entire logical message, including buffer pressure.
    // Receive/liveness processing can therefore be delayed by at most two seconds,
    // not two seconds per fragment of a potentially very large message.
    tokio::time::timeout(MESSAGE_SEND_TIMEOUT, async {
        for packet in packets {
            send(packet).await?;
        }
        Ok::<(), anyhow::Error>(())
    })
    .await?
}

pub async fn run(config: ConnectorConfig) -> Result<()> {
    let endpoint = origin_url(&config.relay_url, config.allow_http_loopback)?;
    let origin = endpoint.url();
    let url = Url::parse(&endpoint.root_url())?;
    relay_crypto::ensure_private_dir(&config.state_dir)?;
    let _state_lock = relay_crypto::lock_private(&config.state_dir.join("connector.lock"))?;
    endpoint.bind_state(&config.state_dir)?;
    let identity = Arc::new(Identity::load_or_create(
        &config.state_dir.join("identity.json"),
    )?);
    let machine = identity.machine_id();
    let pairings = Arc::new(Mutex::new(Pairings::load(
        &config.state_dir,
        machine.clone(),
        identity.public_key(),
        config.reset_pairings,
    )?));
    // A real native handshake must precede registration/readiness; never spawn Core.
    Plain::connect(&config.core_socket, false)
        .await
        .map_err(|_| anyhow::anyhow!("local Core handshake failed"))?;
    let core = Core::new(config.core_socket.clone());
    let monitor = tokio::spawn(core.clone().monitor());
    let mut socket_url = url.clone();
    socket_url
        .set_scheme(if url.scheme() == "https" { "wss" } else { "ws" })
        .map_err(|_| anyhow::anyhow!("invalid relay scheme"))?;
    socket_url.set_path(&endpoint.path("/relay/socket"));
    // Do not expose pairing credentials to captured/non-interactive logs. Tests can
    // read owner-only pairing.json; interactive sessions get the pinned native QR.
    let dashboard = if std::io::stdout().is_terminal() {
        let mut capability = serde_json::to_value(&pairings.lock().await.capability)?;
        if endpoint.base_path == "/main/" {
            capability["channel"] = json!("main");
            capability["basePath"] = json!("/main/");
        }
        let fragment = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&capability)?);
        Some(crate::dashboard::Dashboard::start(
            &format!("{}#pair={fragment}", endpoint.root_url()),
            &config.core_socket.to_string_lossy(),
            config.no_dashboard,
            crate::dashboard::Mode::Relay,
        )?)
    } else {
        None
    };
    let mut backoff = 1u64;
    let mut stop = Box::pin(shutdown_signal());
    loop {
        let mut request = socket_url.as_str().into_client_request()?;
        // HTTP Origin remains authority-only; signed transcripts use endpoint.url().
        request
            .headers_mut()
            .insert("Origin", endpoint.origin.parse()?);
        let connected =
            tokio::select! { _ = &mut stop => break, result = connect_async(request) => result };
        if let Ok((socket, _)) = connected {
            if let Some(card) = &dashboard {
                card.log("Relay connected; authenticating registration.");
            }
            let outcome = relay_connection(
                socket,
                identity.clone(),
                origin.clone(),
                machine.clone(),
                pairings.clone(),
                core.clone(),
                &config,
                dashboard.as_ref(),
                &mut stop,
            )
            .await;
            clear_ready(&config.ready_file);
            match outcome {
                Ok(true) => break,
                Ok(false) => {
                    backoff = 1;
                }
                Err(_) => {}
            }
        }
        if let Some(card) = &dashboard {
            card.log("Relay connection lost. Reconnecting without replaying application input.");
        }
        tokio::select! { _ = &mut stop => break, _ = tokio::time::sleep(Duration::from_secs(backoff)) => {} }
        backoff = (backoff * 2).min(30);
    }
    clear_ready(&config.ready_file);
    monitor.abort();
    Ok(())
}
#[allow(clippy::too_many_arguments)] // One connection borrows the process identity, state and shutdown future.
async fn relay_connection<S>(
    mut socket: tokio_tungstenite::WebSocketStream<S>,
    identity: Arc<Identity>,
    origin: String,
    machine: String,
    pairings: Arc<Mutex<Pairings>>,
    core: Arc<Core>,
    config: &ConnectorConfig,
    dashboard: Option<&crate::dashboard::Dashboard>,
    stop: &mut std::pin::Pin<Box<impl std::future::Future<Output = ()>>>,
) -> Result<bool>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let challenge = tokio::select! {
        _ = &mut *stop => return Ok(true),
        value = tokio::time::timeout(Duration::from_secs(10), socket.next()) => value?.context("relay closed")??,
    };
    let challenge: Value = serde_json::from_str(challenge.to_text()?)?;
    ensure!(challenge["type"] == "challenge", "missing relay challenge");
    let nonce = challenge["nonce"]
        .as_str()
        .context("missing challenge nonce")?;
    ensure!(
        (16..=256).contains(&nonce.len()) && !nonce.contains('\n'),
        "invalid relay challenge"
    );
    let version = env!("CARGO_PKG_VERSION");
    let signature = identity
        .sign(format!("herdr-register-v1\n{origin}\n{machine}\n{nonce}\n{version}").as_bytes());
    socket.send(Message::Text(json!({"type":"register","machineId":machine,"publicKey":identity.public_key(),"version":version,"nonce":nonce,"signature":signature}).to_string().into())).await?;
    let registered = tokio::select! {
        _ = &mut *stop => return Ok(true),
        value = tokio::time::timeout(Duration::from_secs(10), socket.next()) => value?.context("relay closed")??,
    };
    let registered: Value = serde_json::from_str(registered.to_text()?)?;
    ensure!(
        registered["type"] == "registered" && registered["machineId"] == machine,
        "registration rejected"
    );
    // Registration may follow a long reconnect backoff: do not reuse a stale
    // startup handshake as evidence that Core is still available for readiness.
    tokio::select! {
        _ = &mut *stop => return Ok(true),
        result = Plain::connect(&config.core_socket, false) => {
            result.map_err(|_| anyhow::anyhow!("local Core readiness handshake failed"))?;
        }
    }
    if let Some(path) = &config.ready_file {
        let mut ready = json!({"schemaVersion":1,"pid":std::process::id(),"version":version,"protocolVersion":1,"ready":true});
        if crate::relay_channel::RelayEndpoint::parse(
            &config.relay_url,
            config.allow_http_loopback,
        )?
        .base_path
            == "/main/"
        {
            ready["channel"] = json!("main");
            ready["revision"] = json!(crate::build_revision().unwrap_or(""));
        }
        relay_crypto::write_private(path, &serde_json::to_vec(&ready)?)?;
    }
    if let Some(card) = dashboard {
        card.log(
            "Relay registration authenticated; Core ready. Scan the pairing QR in your browser.",
        );
    }
    let (outgoing, mut output) = mpsc::channel(64);
    let endpoint = Arc::new(Endpoint {
        identity,
        origin,
        machine,
        pairings,
        core,
        force_turn: config.force_turn,
        outgoing,
    });
    let mut sessions = HashMap::<String, mpsc::Sender<Value>>::new();
    let mut workers = JoinSet::new();
    let mut shutdown = false;
    let result: Result<()> = async {
        loop { tokio::select! {
            _ = &mut *stop => { shutdown = true; break; },
            Some(value) = output.recv() => { tokio::time::timeout(Duration::from_secs(5), socket.send(Message::Text(value.to_string().into()))).await??; },
            Some(done) = workers.join_next(), if !workers.is_empty() => { if let Ok(id) = done { sessions.remove(&id); } },
            message = socket.next() => {
                let Some(message) = message else { break; };
                let message = message?;
                if let Message::Ping(bytes) = message { socket.send(Message::Pong(bytes)).await?; continue; }
                if matches!(message, Message::Close(_)) { break; }
                ensure!(message.len() <= 700 * 1024, "relay message too large");
                let value: Value = serde_json::from_str(message.to_text()?)?;
                let id = value["sessionId"].as_str().unwrap_or("").to_owned();
                match value["type"].as_str() {
                    Some("session") => {
                        ensure!(!id.is_empty() && id.len() <= 128 && !id.contains('\n') && value["machineId"] == endpoint.machine && !sessions.contains_key(&id), "invalid relay session");
                        if sessions.len() >= 4 { continue; }
                        let (tx, rx) = mpsc::channel(32); sessions.insert(id.clone(), tx);
                        let endpoint = endpoint.clone(); workers.spawn(async move { session(endpoint, id.clone(), rx).await; id });
                    },
                    Some("signal" | "ice") => {
                        if let Some(tx) = sessions.get(&id) && tx.try_send(value).is_err() { sessions.remove(&id); }
                    },
                    Some("ended") => { sessions.remove(&id); },
                    _ => bail!("unexpected relay message"),
                }
            },
        } }
        Ok(())
    }.await;
    sessions.clear();
    // Dropping each route lets every worker close its PC and abort Core handlers.
    let drained = tokio::time::timeout(Duration::from_secs(3), async {
        while workers.join_next().await.is_some() {}
    })
    .await;
    if drained.is_err() {
        workers.abort_all();
    }
    let _ = socket.close(None).await;
    result?;
    Ok(shutdown)
}
fn clear_ready(path: &Option<PathBuf>) {
    if let Some(path) = path
        && let Ok(bytes) = relay_crypto::read_private(path, 4096)
        && let Ok(value) = serde_json::from_slice::<Value>(&bytes)
        && value["pid"] == std::process::id()
    {
        let _ = std::fs::remove_file(path);
    }
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        if let Ok(mut term) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn channel_budget_is_concurrent_and_released_on_handler_abort() {
        let budget = Arc::new(Semaphore::new(MAX_CONCURRENT_CHANNELS));
        // Sequential channels never exhaust a session, even beyond the old cap.
        for _ in 0..100 {
            let permit = budget.clone().try_acquire_owned().unwrap();
            drop(permit);
        }
        let mut permits = Vec::new();
        for _ in 0..MAX_CONCURRENT_CHANNELS {
            permits.push(budget.clone().try_acquire_owned().unwrap());
        }
        assert!(budget.clone().try_acquire_owned().is_err());
        let permit = permits.pop().unwrap();
        let handler = tokio::spawn(async move {
            let _permit = permit;
            std::future::pending::<()>().await;
        });
        handler.abort();
        assert!(handler.await.unwrap_err().is_cancelled());
        let replacement = budget.clone().try_acquire_owned().unwrap();
        assert!(budget.clone().try_acquire_owned().is_err());
        drop(replacement);
        drop(permits);
        assert_eq!(budget.available_permits(), MAX_CONCURRENT_CHANNELS);
    }
    #[tokio::test]
    async fn logical_send_deadline_is_not_reset_between_fragments() {
        use std::sync::atomic::AtomicUsize;
        let completed = AtomicUsize::new(0);
        let result = send_packets(vec![vec![], vec![], vec![]], |_| async {
            tokio::time::sleep(Duration::from_millis(1100)).await;
            completed.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .await;
        assert!(result.is_err());
        assert_eq!(completed.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn relay_origin_policy() {
        assert!(origin_url("https://example.com", false).is_ok());
        let main = origin_url("https://example.com/main/", false).unwrap();
        assert_eq!(main.origin, "https://example.com");
        assert_eq!(main.url(), "https://example.com/main");
        assert_eq!(main.path("/relay/socket"), "/main/relay/socket");
        assert!(origin_url("http://example.com", true).is_err());
        assert!(origin_url("http://127.0.0.1:7777", true).is_ok());
        assert!(origin_url("https://user@example.com", false).is_err());
        assert!(origin_url("https://example.com/path", false).is_err());
    }
    #[test]
    fn signed_envelopes_reject_replay_and_mutation() {
        let dir = tempfile::Builder::new()
            .tempdir_in(std::fs::canonicalize(std::env::temp_dir()).unwrap())
            .unwrap();
        std::fs::set_permissions(
            dir.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        let identity = Identity::load_or_create(&dir.path().join("identity.json")).unwrap();
        let body = "{ \"kind\": \"offer\", \"sdp\": \"opaque\" }";
        let signature = identity.sign(transcript("https://r", "m", "s", 1, body).as_bytes());
        let value = json!({"kind":"signed","seq":1,"body":body,"signature":signature});
        let mut last = 0;
        assert_eq!(
            verify_envelope(
                &value,
                &identity.public_key(),
                "https://r",
                "m",
                "s",
                &mut last
            )
            .unwrap()["kind"],
            "offer"
        );
        assert!(
            verify_envelope(
                &value,
                &identity.public_key(),
                "https://r",
                "m",
                "s",
                &mut last
            )
            .is_err()
        );
        assert!(
            verify_envelope(
                &value,
                &identity.public_key(),
                "https://r/main",
                "m",
                "s",
                &mut 0
            )
            .is_err()
        );
        let mut altered = value.clone();
        altered["body"] = json!("{\"kind\":\"offer\",\"sdp\":\"opaque\"}");
        assert!(
            verify_envelope(
                &altered,
                &identity.public_key(),
                "https://r",
                "m",
                "s",
                &mut 0
            )
            .is_err()
        );
        assert!(
            verify_envelope(
                &value,
                &identity.public_key(),
                "https://r",
                "m",
                "other",
                &mut 0
            )
            .is_err()
        );
    }
    #[test]
    fn transcript_uses_exact_bytes() {
        assert_eq!(
            transcript("https://r", "m", "s", 2, "{ \"kind\": 1 }"),
            "herdr-signal-v1\nhttps://r\nm\ns\n2\n{ \"kind\": 1 }"
        );
    }
}
