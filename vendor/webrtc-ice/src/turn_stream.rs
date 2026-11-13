// Local extension to webrtc-ice 0.17.3. See PATCHES.md.
//! TURN stream framing (RFC 8489 / RFC 8656), deliberately not RFC 4571.
use std::{any::Any, io, net::SocketAddr, sync::Mutex as StdMutex, time::Duration};

use async_trait::async_trait;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
    sync::{mpsc, oneshot, watch, Mutex},
    task::JoinHandle,
};
use tokio_rustls::{rustls, TlsConnector};
use util::Conn;

// turn::client's receive buffer is u16::MAX. Include headers and TCP padding.
const MAX_FRAME: usize = u16::MAX as usize;
const QUEUE: usize = 8;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
type WriteRequest = (Vec<u8>, oneshot::Sender<io::Result<usize>>);

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn closed() -> io::Error {
    io::Error::new(io::ErrorKind::NotConnected, "TURN stream closed")
}

// Length on the wire, and length exposed to the packet-oriented TURN client.
fn frame_lengths(header: &[u8]) -> io::Result<(usize, usize)> {
    let length = u16::from_be_bytes([header[2], header[3]]) as usize;
    let (wire, packet) = match header[0] >> 6 {
        0 => {
            if !length.is_multiple_of(4) {
                return Err(invalid("unaligned STUN length"));
            }
            (20 + length, 20 + length)
        }
        1 => (4 + ((length + 3) & !3), 4 + length),
        _ => return Err(invalid("invalid TURN stream prefix")),
    };
    if wire > MAX_FRAME {
        return Err(invalid("TURN frame exceeds client receive buffer"));
    }
    Ok((wire, packet))
}

async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> io::Result<Vec<u8>> {
    let mut header = [0; 4];
    reader.read_exact(&mut header).await?;
    let (wire, packet) = frame_lengths(&header)?;
    let mut frame = vec![0; wire];
    frame[..4].copy_from_slice(&header);
    reader.read_exact(&mut frame[4..]).await?;
    if header[0] >> 6 == 0 && frame[4..8] != [0x21, 0x12, 0xa4, 0x42] {
        return Err(invalid("invalid STUN magic cookie"));
    }
    frame.truncate(packet);
    Ok(frame)
}

fn tls_config(roots: rustls::RootCertStore) -> io::Result<rustls::ClientConfig> {
    Ok(
        rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|e| invalid(&e.to_string()))?
        .with_root_certificates(roots)
        .with_no_client_auth(),
    )
}

async fn connect_tls(
    stream: TcpStream,
    host: &str,
    config: rustls::ClientConfig,
) -> io::Result<tokio_rustls::client::TlsStream<TcpStream>> {
    let name = rustls::pki_types::ServerName::try_from(host.to_owned())
        .map_err(|e| invalid(&e.to_string()))?;
    TlsConnector::from(std::sync::Arc::new(config))
        .connect(name, stream)
        .await
}

/// Independent bounded reader/writer workers make cancellation of a caller's
/// recv or send harmless to framing. A queued send is completed, never retried.
/// Dropping or closing this object stops both workers and releases both halves.
pub(crate) struct TurnStreamConn {
    local: SocketAddr,
    peer: SocketAddr,
    incoming: Mutex<mpsc::Receiver<io::Result<Vec<u8>>>>,
    outgoing: mpsc::Sender<WriteRequest>,
    shutdown: watch::Sender<bool>,
    workers: StdMutex<Vec<JoinHandle<()>>>,
}

impl TurnStreamConn {
    pub(crate) async fn dial(host: &str, port: u16, tls: bool) -> io::Result<Self> {
        tokio::time::timeout(CONNECT_TIMEOUT, async {
            let stream = TcpStream::connect((host, port)).await?;
            let local = stream.local_addr()?;
            let peer = stream.peer_addr()?;
            stream.set_nodelay(true)?;
            if tls {
                let mut roots = rustls::RootCertStore::empty();
                let native = rustls_native_certs::load_native_certs();
                for cert in native.certs {
                    roots.add(cert).map_err(|e| invalid(&e.to_string()))?;
                }
                if roots.is_empty() {
                    return Err(invalid("no system TLS trust roots available"));
                }
                let stream = connect_tls(stream, host, tls_config(roots)?).await?;
                Ok(Self::new(stream, local, peer))
            } else {
                Ok(Self::new(stream, local, peer))
            }
        })
        .await
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "TURN connect/TLS handshake timed out",
            )
        })?
    }

    fn new<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
        stream: S,
        local: SocketAddr,
        peer: SocketAddr,
    ) -> Self {
        let (mut reader, mut writer) = tokio::io::split(stream);
        let (tx, incoming) = mpsc::channel(QUEUE);
        let (outgoing, mut rx) = mpsc::channel::<WriteRequest>(QUEUE);
        let (shutdown, mut read_stop) = watch::channel(false);
        let mut write_stop = shutdown.subscribe();
        let reader_task = tokio::spawn(async move {
            loop {
                let frame = tokio::select! {
                    biased;
                    _ = read_stop.changed() => break,
                    frame = read_frame(&mut reader) => frame,
                };
                let failed = frame.is_err();
                tokio::select! {
                    biased;
                    _ = read_stop.changed() => break,
                    result = tx.send(frame) => if result.is_err() { break; },
                }
                if failed {
                    break;
                }
            }
        });
        let writer_task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = write_stop.changed() => break,
                    request = rx.recv() => {
                        let Some((frame, done)) = request else { break; };
                        let result = tokio::select! {
                            biased;
                            _ = write_stop.changed() => break,
                            result = async {
                                writer.write_all(&frame).await?;
                                writer.flush().await?;
                                io::Result::Ok(frame.len())
                            } => result,
                        };
                        let failed = result.is_err();
                        let _ = done.send(result);
                        if failed { break; }
                    }
                }
            }
        });
        Self {
            local,
            peer,
            incoming: Mutex::new(incoming),
            outgoing,
            shutdown,
            workers: StdMutex::new(vec![reader_task, writer_task]),
        }
    }
}

impl Drop for TurnStreamConn {
    fn drop(&mut self) {
        self.shutdown.send_replace(true);
        for worker in self.workers.get_mut().unwrap().drain(..) {
            worker.abort();
        }
    }
}

#[async_trait]
impl Conn for TurnStreamConn {
    async fn connect(&self, addr: SocketAddr) -> util::Result<()> {
        if addr != self.peer {
            return Err(invalid("TURN stream destination differs from connected peer").into());
        }
        Ok(())
    }
    async fn recv(&self, buf: &mut [u8]) -> util::Result<usize> {
        let mut stop = self.shutdown.subscribe();
        if *stop.borrow() {
            return Err(closed().into());
        }
        let frame = tokio::select! {
            biased;
            _ = stop.changed() => return Err(closed().into()),
            frame = async { self.incoming.lock().await.recv().await } => frame.ok_or_else(closed)??,
        };
        if frame.len() > buf.len() {
            return Err(invalid("TURN receive buffer too small").into());
        }
        buf[..frame.len()].copy_from_slice(&frame);
        Ok(frame.len())
    }
    async fn recv_from(&self, buf: &mut [u8]) -> util::Result<(usize, SocketAddr)> {
        Ok((self.recv(buf).await?, self.peer))
    }
    async fn send(&self, buf: &[u8]) -> util::Result<usize> {
        if buf.len() < 4 {
            return Err(invalid("short TURN frame").into());
        }
        let (wire, packet) = frame_lengths(buf)?;
        if buf.len() != packet && buf.len() != wire {
            return Err(invalid("invalid TURN frame length").into());
        }
        if buf[0] >> 6 == 0 && (buf.len() < 20 || buf[4..8] != [0x21, 0x12, 0xa4, 0x42]) {
            return Err(invalid("invalid STUN header").into());
        }
        let mut frame = buf.to_vec();
        frame.resize(wire, 0);
        let (done, result) = oneshot::channel();
        let mut stop = self.shutdown.subscribe();
        if *stop.borrow() {
            return Err(closed().into());
        }
        tokio::select! {
            biased;
            _ = stop.changed() => return Err(closed().into()),
            sent = self.outgoing.send((frame, done)) => sent.map_err(|_| closed())?,
        }
        tokio::select! {
            biased;
            _ = stop.changed() => Err(closed().into()),
            result = result => { result.map_err(|_| closed())??; Ok(buf.len()) },
        }
    }
    async fn send_to(&self, buf: &[u8], target: SocketAddr) -> util::Result<usize> {
        self.connect(target).await?;
        self.send(buf).await
    }
    fn local_addr(&self) -> util::Result<SocketAddr> {
        Ok(self.local)
    }
    fn remote_addr(&self) -> Option<SocketAddr> {
        Some(self.peer)
    }
    async fn close(&self) -> util::Result<()> {
        self.shutdown.send_replace(true);
        // Abort synchronously as well: no cancelled close can strand a descriptor.
        for worker in self.workers.lock().unwrap().drain(..) {
            worker.abort();
        }
        Ok(())
    }
    fn as_any(&self) -> &(dyn Any + Send + Sync) {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        pin::Pin,
        sync::Arc,
        task::{Context, Poll},
    };
    use tokio::io::{duplex, DuplexStream, ReadBuf};

    fn addresses() -> (SocketAddr, SocketAddr) {
        (
            "127.0.0.1:1000".parse().unwrap(),
            "127.0.0.1:3478".parse().unwrap(),
        )
    }
    fn stun() -> Vec<u8> {
        let mut frame = vec![0; 20];
        frame[1] = 1;
        frame[4..8].copy_from_slice(&[0x21, 0x12, 0xa4, 0x42]);
        frame
    }
    fn pair(capacity: usize) -> (Arc<TurnStreamConn>, DuplexStream) {
        let (a, b) = duplex(capacity);
        let (local, peer) = addresses();
        (Arc::new(TurnStreamConn::new(a, local, peer)), b)
    }

    #[tokio::test]
    async fn split_coalesced_stun_and_channel_padding() {
        let (conn, mut server) = pair(128);
        let frame = stun();
        server.write_all(&frame[..3]).await.unwrap();
        // A cancelled recv must not lose the partial STUN header.
        let mut buf = vec![0; MAX_FRAME];
        assert!(
            tokio::time::timeout(Duration::from_millis(10), conn.recv(&mut buf))
                .await
                .is_err()
        );
        let mut rest = frame[3..].to_vec();
        rest.extend([0x40, 0x01, 0, 1, 0xaa, 0, 0, 0]);
        rest.extend(&frame);
        server.write_all(&rest).await.unwrap();
        assert_eq!(conn.recv(&mut buf).await.unwrap(), 20);
        assert_eq!(&buf[..20], frame);
        assert_eq!(conn.recv(&mut buf).await.unwrap(), 5);
        assert_eq!(&buf[..5], &[0x40, 1, 0, 1, 0xaa]);
        assert_eq!(conn.recv(&mut buf).await.unwrap(), 20);
        assert_eq!(&buf[..20], frame);
        conn.close().await.unwrap();
    }

    #[tokio::test]
    async fn malformed_prefix_length_cookie_and_eof() {
        for bytes in [
            vec![0x80, 0, 0, 0],
            vec![0, 1, 0, 1],
            vec![0, 1, 0xff, 0xfc],
            vec![0x40, 1, 0xff, 0xff],
            vec![0; 20],
            vec![0, 1],
            stun()[..19].to_vec(),
            vec![],
        ] {
            let (conn, mut server) = pair(128);
            server.write_all(&bytes).await.unwrap();
            drop(server);
            assert!(conn.recv(&mut [0; 64]).await.is_err(), "{bytes:?}");
            conn.close().await.unwrap();
        }
    }

    // Force short writes, even when the underlying duplex has spare capacity.
    struct ShortIo(DuplexStream);
    impl AsyncRead for ShortIo {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            Pin::new(&mut self.0).poll_read(cx, buf)
        }
    }
    impl AsyncWrite for ShortIo {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            Pin::new(&mut self.0).poll_write(cx, &buf[..buf.len().min(2)])
        }
        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.0).poll_flush(cx)
        }
        fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.0).poll_shutdown(cx)
        }
    }

    #[tokio::test]
    async fn concurrent_short_writes_and_destination() {
        let (stream, mut server) = duplex(128);
        let (local, peer) = addresses();
        let conn = Arc::new(TurnStreamConn::new(ShortIo(stream), local, peer));
        let mut tasks = vec![];
        for n in 0..12 {
            let conn = conn.clone();
            tasks.push(tokio::spawn(async move {
                conn.send_to(&[0x40, 1, 0, 1, n], peer).await.unwrap()
            }));
        }
        let mut seen = vec![];
        for _ in 0..12 {
            let frame = read_frame(&mut server).await.unwrap();
            assert_eq!(&frame[..4], &[0x40, 1, 0, 1]);
            seen.push(frame[4]);
        }
        seen.sort();
        assert_eq!(seen, (0..12).collect::<Vec<_>>());
        for task in tasks {
            assert_eq!(task.await.unwrap(), 5);
        }
        assert!(conn.send_to(&stun(), local).await.is_err());
        for bad in [vec![], vec![0; 3], vec![0x40, 1, 0, 2, 0], vec![0; 20]] {
            assert!(conn.send(&bad).await.is_err());
        }
        conn.close().await.unwrap();
    }

    #[tokio::test]
    async fn cancelled_write_finishes_frame_and_close_wakes_io() {
        let (conn, mut server) = pair(1);
        let sender = {
            let conn = conn.clone();
            tokio::spawn(async move { conn.send(&stun()).await })
        };
        let mut first = [0; 1];
        server.read_exact(&mut first).await.unwrap();
        sender.abort();
        let mut rest = [0; 19];
        server.read_exact(&mut rest).await.unwrap();
        let mut actual = first.to_vec();
        actual.extend(rest);
        assert_eq!(actual, stun());
        let reader = {
            let conn = conn.clone();
            tokio::spawn(async move { conn.recv(&mut [0; 64]).await })
        };
        let sender = {
            let conn = conn.clone();
            tokio::spawn(async move { conn.send(&stun()).await })
        };
        server.read_exact(&mut first).await.unwrap();
        conn.close().await.unwrap();
        conn.close().await.unwrap();
        assert!(reader.await.unwrap().is_err());
        assert!(sender.await.unwrap().is_err());
        let mut rest = vec![];
        tokio::time::timeout(Duration::from_secs(1), server.read_to_end(&mut rest))
            .await
            .unwrap()
            .unwrap();
        assert!(conn.send(&stun()).await.is_err());
        assert!(conn.recv(&mut [0; 64]).await.is_err());
    }

    async fn tls_fixture(host: &str, trusted: bool) -> io::Result<()> {
        let rcgen::CertifiedKey { cert, key_pair } = rcgen::generate_simple_self_signed(vec![
            "localhost".to_owned(),
            "127.0.0.1".to_owned(),
        ])
        .unwrap();
        let certificate = cert.der().clone();
        let key = rustls::pki_types::PrivatePkcs8KeyDer::from(key_pair.serialize_der());
        let server_config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![certificate.clone()], key.into())
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_config));
            if let Ok(mut tls) = acceptor.accept(stream).await {
                let _ = tls.write_all(&stun()).await;
            }
        });
        let mut roots = rustls::RootCertStore::empty();
        if trusted {
            roots.add(certificate).unwrap();
        }
        let result = async {
            let stream = TcpStream::connect(addr).await?;
            let mut stream = connect_tls(stream, host, tls_config(roots)?).await?;
            assert_eq!(read_frame(&mut stream).await?, stun());
            Ok(())
        }
        .await;
        server.await.unwrap();
        result
    }

    #[tokio::test]
    async fn tls_verifies_original_host_ip_and_trust() {
        tls_fixture("localhost", true).await.unwrap();
        tls_fixture("127.0.0.1", true).await.unwrap();
        assert!(tls_fixture("wrong.example", true).await.is_err());
        assert!(tls_fixture("localhost", false).await.is_err());
    }

    #[tokio::test]
    async fn tls_plaintext_rejected_no_downgrade() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            stream.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await.unwrap();
            // No second connection or plaintext TURN retry may occur.
            assert!(
                tokio::time::timeout(Duration::from_millis(100), listener.accept())
                    .await
                    .is_err()
            );
        });
        assert!(TurnStreamConn::dial("127.0.0.1", addr.port(), true)
            .await
            .is_err());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn stalled_tls_handshake_is_bounded() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.unwrap();
            std::future::pending::<()>().await;
        });
        let start = tokio::time::Instant::now();
        let err = TurnStreamConn::dial("127.0.0.1", addr.port(), true)
            .await
            .err()
            .unwrap();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert!(start.elapsed() < CONNECT_TIMEOUT + Duration::from_secs(2));
        server.abort();
        // Connection refusal is also surfaced, never replaced by a UDP attempt.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        assert!(TurnStreamConn::dial("127.0.0.1", addr.port(), false)
            .await
            .is_err());
    }
}
