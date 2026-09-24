//! Phase 8 QUIC framing (S06 boundary).
//!
//! Seven does NOT extend theMQL's `TransportKind`. This module is a projection:
//! a `themql_core::Message` is serialized as JSON (`FormatTag::Json` is
//! first-class upstream) and framed with a 4-byte big-endian length prefix for
//! transport over a QUIC bidirectional stream.
//!
//! JSON (not postcard) is the framing codec here because `Message.metadata`
//! carries `serde_json::Value` extensions, which postcard cannot represent.
//! The canonical evidence bytes *inside* the message remain postcard (decision
//! 0001); only the envelope is JSON.
//!
//! The socket layer below the framing (`QuinnEndpoint`, `send_message`,
//! `recv_message`) runs framed messages over QUIC bidirectional streams.
//! TLS identity is caller-supplied (`quinn::ServerConfig` / client config):
//! Seven never mints deployment credentials — tests mint throwaway
//! self-signed certs via `rcgen` (dev-dependency, never a runtime dep).

use crate::{MqlError, MqlResult};
use std::net::SocketAddr;
use themql_core::Message;

/// Maximum framed `Message` size accepted on decode (8 MiB). QUIC has no `LoRa`
/// byte budget, but an explicit bound keeps a corrupt length prefix from
/// allocating unbounded memory (§18 fail-explicit).
pub const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;

/// QUIC stream configuration. Documents intent; socket wiring consumes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuicAdapterConfig {
    /// Maximum single `Message` size in bytes (JSON form).
    pub max_message_bytes: usize,
    /// Whether to keep the stream open for multiple framed messages.
    pub keep_stream_open: bool,
}

impl Default for QuicAdapterConfig {
    fn default() -> Self {
        Self {
            max_message_bytes: MAX_MESSAGE_BYTES,
            keep_stream_open: true,
        }
    }
}

/// Encode one `Message` as `[len_be32][json_bytes]`.
pub fn encode_frame(msg: &Message, config: &QuicAdapterConfig) -> MqlResult<Vec<u8>> {
    let body = serde_json::to_vec(msg)
        .map_err(|e| MqlError::Quic(format!("json encode failed: {e}")))?;
    if body.len() > config.max_message_bytes {
        return Err(MqlError::Quic(format!(
            "message {} B exceeds max {} B",
            body.len(),
            config.max_message_bytes
        )));
    }
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

/// Decode one framed `Message` from the front of `buf`, returning the message
/// and the number of bytes consumed. `Ok(None)` means "need more bytes".
pub fn decode_frame(buf: &[u8], config: &QuicAdapterConfig) -> MqlResult<Option<(Message, usize)>> {
    if buf.len() < 4 {
        return Ok(None);
    }
    let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    if len > config.max_message_bytes {
        return Err(MqlError::Quic(format!(
            "frame length {len} B exceeds max {} B",
            config.max_message_bytes
        )));
    }
    if buf.len() < 4 + len {
        return Ok(None);
    }
    let msg: Message = serde_json::from_slice(&buf[4..4 + len])
        .map_err(|e| MqlError::Quic(format!("json decode failed: {e}")))?;
    Ok(Some((msg, 4 + len)))
}

// ===========================================================================
// Socket layer — framed Messages over QUIC bidirectional streams (quinn)
// ===========================================================================

/// A bound QUIC endpoint. Created with caller-supplied TLS identity for
/// serving, or without for client-only use.
pub struct QuinnEndpoint {
    inner: quinn::Endpoint,
}

impl QuinnEndpoint {
    /// Bind `addr` (use port 0 for an ephemeral loopback port). `server`
    /// enables accepting inbound connections; `None` is client-only.
    ///
    /// # Errors
    /// Propagates socket/bind failures verbatim (§18).
    pub fn bind(addr: SocketAddr, server: Option<quinn::ServerConfig>) -> MqlResult<Self> {
        let endpoint = match server {
            Some(config) => quinn::Endpoint::server(config, addr),
            None => quinn::Endpoint::client(addr),
        }
        .map_err(|e| MqlError::Quic(format!("bind {addr} failed: {e}")))?;
        Ok(Self { inner: endpoint })
    }

    /// Set the default client configuration (roots, keys) for outbound
    /// connections from this endpoint.
    pub fn set_default_client_config(&mut self, config: quinn::ClientConfig) {
        self.inner.set_default_client_config(config);
    }

    /// Local address bound (useful with port 0).
    ///
    /// # Errors
    /// Propagates socket introspection failures.
    pub fn local_addr(&self) -> MqlResult<SocketAddr> {
        self.inner
            .local_addr()
            .map_err(|e| MqlError::Quic(format!("local_addr failed: {e}")))
    }

    /// Connect outbound to `addr` (TLS `server_name` for verification).
    ///
    /// # Errors
    /// Connection establishment failures.
    pub async fn connect(&self, addr: SocketAddr, server_name: &str) -> MqlResult<quinn::Connection> {
        self.inner
            .connect(addr, server_name)
            .map_err(|e| MqlError::Quic(format!("connect {addr} failed: {e}")))?
            .await
            .map_err(|e| MqlError::Quic(format!("handshake {addr} failed: {e}")))
    }

    /// Accept one inbound connection.
    ///
    /// # Errors
    /// Accept/handshake failures. Returns `None` only when the endpoint is
    /// closed — surfaced as an explicit error, never a silent stall.
    pub async fn accept(&self) -> MqlResult<quinn::Connection> {
        let incoming = self
            .inner
            .accept()
            .await
            .ok_or_else(|| MqlError::Quic("endpoint closed while accepting".to_string()))?;
        incoming
            .await
            .map_err(|e| MqlError::Quic(format!("inbound handshake failed: {e}")))
    }
}

/// Load a QUIC server config from operator-provisioned PEM files (cert chain
/// + exactly one private key). Seven never mints deployment credentials —
/// this only loads what the operator provisions (PEM paths are deployment
/// configuration, like the TLS material itself).
///
/// # Errors
/// Missing/unreadable files, unparseable PEM, empty chain, or missing key.
pub fn load_server_config_from_pem(
    cert_chain_path: &std::path::Path,
    private_key_path: &std::path::Path,
) -> MqlResult<quinn::ServerConfig> {
    use std::io::BufReader;

    let cert_file = std::fs::File::open(cert_chain_path)
        .map_err(|e| MqlError::Quic(format!("open {} failed: {e}", cert_chain_path.display())))?;
    let certs: Vec<_> = rustls_pemfile::certs(&mut BufReader::new(cert_file))
        .collect::<Result<_, _>>()
        .map_err(|e| MqlError::Quic(format!("parse {} failed: {e}", cert_chain_path.display())))?;
    if certs.is_empty() {
        return Err(MqlError::Quic(format!(
            "no certificates in {}",
            cert_chain_path.display()
        )));
    }
    let key_file = std::fs::File::open(private_key_path).map_err(|e| {
        MqlError::Quic(format!("open {} failed: {e}", private_key_path.display()))
    })?;
    let key = rustls_pemfile::private_key(&mut BufReader::new(key_file))
        .map_err(|e| MqlError::Quic(format!("parse {} failed: {e}", private_key_path.display())))?
        .ok_or_else(|| {
            MqlError::Quic(format!("no private key in {}", private_key_path.display()))
        })?;
    quinn::ServerConfig::with_single_cert(certs, key)
        .map_err(|e| MqlError::Quic(format!("server config failed: {e}")))
}

/// Send one framed `Message` over a fresh bidirectional stream, then
/// gracefully finish the send side. One message per stream keeps framing
/// trivially parseable; batching via [`send_messages`] reuses one stream.
///
/// # Errors
/// Stream open/write/finish failures, or oversize messages.
pub async fn send_message(
    conn: &quinn::Connection,
    msg: &Message,
    config: &QuicAdapterConfig,
) -> MqlResult<()> {
    let bytes = encode_frame(msg, config)?;
    let mut send = conn
        .open_bi()
        .await
        .map_err(|e| MqlError::Quic(format!("open_bi failed: {e}")))?;
    send.0
        .write_all(&bytes)
        .await
        .map_err(|e| MqlError::Quic(format!("write failed: {e}")))?;
    send
        .0
        .finish()
        .map_err(|e| MqlError::Quic(format!("finish failed: {e}")))?;
    Ok(())
}

/// Accept one bidirectional stream and decode one framed `Message`.
///
/// # Errors
/// Accept/read/decode failures, or frames exceeding `config.max_message_bytes`.
pub async fn recv_message(
    conn: &quinn::Connection,
    config: &QuicAdapterConfig,
) -> MqlResult<Message> {
    let mut streams = conn
        .accept_bi()
        .await
        .map_err(|e| MqlError::Quic(format!("accept_bi failed: {e}")))?;
    let mut len_buf = [0u8; 4];
    streams
        .1
        .read_exact(&mut len_buf)
        .await
        .map_err(|e| MqlError::Quic(format!("read length failed: {e}")))?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > config.max_message_bytes {
        return Err(MqlError::Quic(format!(
            "frame length {len} B exceeds max {} B",
            config.max_message_bytes
        )));
    }
    let mut body = vec![0u8; len];
    streams
        .1
        .read_exact(&mut body)
        .await
        .map_err(|e| MqlError::Quic(format!("read body failed: {e}")))?;
    serde_json::from_slice(&body).map_err(|e| MqlError::Quic(format!("json decode failed: {e}")))
}

/// Send a batch of messages over a single stream: `[count_be32][frame ×
/// count]`, then finish. One stream per batch (not per message) amortizes
/// stream setup for node syncs. An explicit count prefix — rather than
/// read-until-finish — keeps framing unambiguous without EOF edge cases.
/// An empty batch is valid and yields zero frames.
///
/// # Errors
/// Stream open/write/finish failures, or any oversize frame.
pub async fn send_messages(
    conn: &quinn::Connection,
    msgs: &[Message],
    config: &QuicAdapterConfig,
) -> MqlResult<()> {
    let count: u32 = msgs
        .len()
        .try_into()
        .map_err(|_| MqlError::Quic(format!("batch too large: {} messages", msgs.len())))?;
    let mut send = conn
        .open_bi()
        .await
        .map_err(|e| MqlError::Quic(format!("open_bi failed: {e}")))?;
    send
        .0
        .write_all(&count.to_be_bytes())
        .await
        .map_err(|e| MqlError::Quic(format!("write count failed: {e}")))?;
    for msg in msgs {
        let bytes = encode_frame(msg, config)?;
        send.0
            .write_all(&bytes)
            .await
            .map_err(|e| MqlError::Quic(format!("write failed: {e}")))?;
    }
    send
        .0
        .finish()
        .map_err(|e| MqlError::Quic(format!("finish failed: {e}")))?;
    Ok(())
}

/// Receive exactly one batch written by [`send_messages`]. Reads the count
/// first, then that many frames — never preallocates by count (a corrupt
/// prefix cannot force allocation; each frame is still size-bounded).
///
/// # Errors
/// Accept/read/decode failures, oversize frames, or truncation mid-batch.
pub async fn recv_messages(
    conn: &quinn::Connection,
    config: &QuicAdapterConfig,
) -> MqlResult<Vec<Message>> {
    let mut streams = conn
        .accept_bi()
        .await
        .map_err(|e| MqlError::Quic(format!("accept_bi failed: {e}")))?;
    let mut count_buf = [0u8; 4];
    streams
        .1
        .read_exact(&mut count_buf)
        .await
        .map_err(|e| MqlError::Quic(format!("read count failed: {e}")))?;
    let count = u32::from_be_bytes(count_buf);
    let mut out = Vec::new();
    for _ in 0..count {
        let mut len_buf = [0u8; 4];
        streams
            .1
            .read_exact(&mut len_buf)
            .await
            .map_err(|e| MqlError::Quic(format!("read length failed: {e}")))?;
        let len = u32::from_be_bytes(len_buf) as usize;
        if len > config.max_message_bytes {
            return Err(MqlError::Quic(format!(
                "frame length {len} B exceeds max {} B",
                config.max_message_bytes
            )));
        }
        let mut body = vec![0u8; len];
        streams
            .1
            .read_exact(&mut body)
            .await
            .map_err(|e| MqlError::Quic(format!("read body failed: {e}")))?;
        out.push(
            serde_json::from_slice(&body)
                .map_err(|e| MqlError::Quic(format!("json decode failed: {e}")))?,
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use seven_core::{CanonicalState, PhysicalObservation, Quaternion, SubjectId};

    fn message() -> Message {
        let state = CanonicalState::canonicalize(&PhysicalObservation {
            position_m: [100.0, 200.0, 300.0],
            velocity_ms: [1.0, 2.0, 3.0],
            attitude: Quaternion::identity(),
            observed_at_nanos: 1000,
        })
        .unwrap();
        let e = seven_evidence::Evidence::originate(
            &SigningKey::from_bytes(&[1; 32]),
            SubjectId("ac1".into()),
            &state,
            1000,
            i64::MAX,
        )
        .unwrap();
        crate::to_message(&e, crate::SEVEN_DOMAIN).unwrap()
    }

    #[test]
    fn frame_roundtrip_preserves_message() {
        let cfg = QuicAdapterConfig::default();
        let m = message();
        let bytes = encode_frame(&m, &cfg).unwrap();
        let (back, consumed) = decode_frame(&bytes, &cfg).unwrap().expect("complete frame");
        assert_eq!(consumed, bytes.len());
        assert_eq!(m, back, "QUIC framing must preserve semantics (inv 12)");
    }

    #[test]
    fn partial_frame_waits_for_more_bytes() {
        let cfg = QuicAdapterConfig::default();
        let bytes = encode_frame(&message(), &cfg).unwrap();
        assert!(decode_frame(&bytes[..2], &cfg).unwrap().is_none());
        assert!(decode_frame(&bytes[..4], &cfg).unwrap().is_none());
        let mid = bytes.len() - 1;
        assert!(decode_frame(&bytes[..mid], &cfg).unwrap().is_none());
    }

    #[test]
    fn oversize_frame_rejected_explicitly() {
        let cfg = QuicAdapterConfig {
            max_message_bytes: 8,
            keep_stream_open: true,
        };
        assert!(matches!(
            encode_frame(&message(), &cfg),
            Err(MqlError::Quic(_))
        ));
        let bad = 1024u32.to_be_bytes();
        assert!(matches!(
            decode_frame(&bad, &cfg),
            Err(MqlError::Quic(_))
        ));
    }

    #[test]
    fn back_to_back_frames_decode_in_order() {
        let cfg = QuicAdapterConfig::default();
        let m = message();
        let a = encode_frame(&m, &cfg).unwrap();
        let b = encode_frame(&m, &cfg).unwrap();
        let mut stream = [a.clone(), b.clone()].concat();
        let (m1, n1) = decode_frame(&stream, &cfg).unwrap().unwrap();
        let (m2, n2) = decode_frame(&stream[n1..], &cfg).unwrap().unwrap();
        assert_eq!(n1 + n2, stream.len());
        assert_eq!(m1, m2);
        let _ = &mut stream;
    }

    /// Loopback pair: bound server + client endpoint trusting the server's
    /// throwaway self-signed cert. Returns `(server, client, server_addr)`.
    /// Join (don't sequence) the connect/accept futures: under a
    /// current-thread runtime the server future must be polled for its side
    /// of the handshake to progress.
    fn loopback_pair() -> (QuinnEndpoint, QuinnEndpoint, std::net::SocketAddr) {
        use std::sync::Arc;

        let certified = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let key = quinn::rustls::pki_types::PrivateKeyDer::Pkcs8(
            certified.signing_key.serialize_der().into(),
        );
        let server_config =
            quinn::ServerConfig::with_single_cert(vec![certified.cert.der().clone()], key).unwrap();
        let mut roots = quinn::rustls::RootCertStore::empty();
        roots.add(certified.cert.der().clone()).unwrap();
        let client_config =
            quinn::ClientConfig::with_root_certificates(Arc::new(roots)).unwrap();

        let server =
            QuinnEndpoint::bind("127.0.0.1:0".parse().unwrap(), Some(server_config)).unwrap();
        let server_addr = server.local_addr().unwrap();
        let mut client_ep =
            QuinnEndpoint::bind("127.0.0.1:0".parse().unwrap(), None).unwrap();
        client_ep.set_default_client_config(client_config);
        (server, client_ep, server_addr)
    }

    /// Loopback over real QUIC (ephemeral 127.0.0.1 ports, throwaway
    /// self-signed cert): send → receive preserves the `Message` bit-for-bit
    /// (invariant 12 across the socket layer, not just the codec).
    #[tokio::test]
    async fn loopback_send_recv_preserves_message() {
        let (server, client_ep, server_addr) = loopback_pair();
        let cfg = QuicAdapterConfig::default();
        let expected = message();
        let (conn_client, conn_server) = tokio::join!(
            client_ep.connect(server_addr, "localhost"),
            server.accept()
        );
        let conn_client = conn_client.unwrap();
        let conn_server = conn_server.unwrap();
        send_message(&conn_client, &expected, &cfg).await.unwrap();
        let got = recv_message(&conn_server, &cfg).await.unwrap();
        assert_eq!(expected, got, "QUIC socket must preserve semantics (inv 12)");
    }

    /// Batch loopback: N messages over one stream arrive in order, plus the
    /// empty batch edge case.
    #[tokio::test]
    async fn loopback_batch_preserves_order() {        let (server, client_ep, server_addr) = loopback_pair();
        let cfg = QuicAdapterConfig::default();
        let expected = vec![message(), message(), message(), message(), message()];
        let (conn_client, conn_server) = tokio::join!(
            client_ep.connect(server_addr, "localhost"),
            server.accept()
        );
        let conn_client = conn_client.unwrap();
        let conn_server = conn_server.unwrap();
        let (send_res, got) = tokio::join!(
            send_messages(&conn_client, &expected, &cfg),
            recv_messages(&conn_server, &cfg)
        );
        send_res.unwrap();
        assert_eq!(expected, got.unwrap(), "batch must preserve order and content");

        let (conn_client2, conn_server2) = tokio::join!(
            client_ep.connect(server_addr, "localhost"),
            server.accept()
        );
        let conn_client2 = conn_client2.unwrap();
        let conn_server2 = conn_server2.unwrap();
        let (send_empty, got_empty) = tokio::join!(
            send_messages(&conn_client2, &[], &cfg),
            recv_messages(&conn_server2, &cfg)
        );
        send_empty.unwrap();
        assert!(got_empty.unwrap().is_empty(), "empty batch round-trips");
    }

    /// Operator TLS path: a self-signed cert written as PEM files loads back
    /// into a working server config (loopback handshake succeeds), and every
    /// failure mode (missing file, empty chain, keyless file) is explicit.
    #[tokio::test]
    async fn pem_server_config_loads_and_serves() {
        use std::io::Write as _;

        let dir = std::env::temp_dir().join(format!(
            "seven-quic-pem-test-{}-{}",
            std::process::id(),
            "loads-and-serves"
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let cert_path = dir.join("chain.pem");
        let key_path = dir.join("key.pem");

        let certified = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        std::fs::File::create(&cert_path)
            .unwrap()
            .write_all(certified.cert.pem().as_bytes())
            .unwrap();
        std::fs::File::create(&key_path)
            .unwrap()
            .write_all(certified.signing_key.serialize_pem().as_bytes())
            .unwrap();

        let server_config = load_server_config_from_pem(&cert_path, &key_path).unwrap();
        let mut roots = quinn::rustls::RootCertStore::empty();
        roots.add(certified.cert.der().clone()).unwrap();
        let client_config =
            quinn::ClientConfig::with_root_certificates(std::sync::Arc::new(roots)).unwrap();

        let server =
            QuinnEndpoint::bind("127.0.0.1:0".parse().unwrap(), Some(server_config)).unwrap();
        let server_addr = server.local_addr().unwrap();
        let mut client_ep =
            QuinnEndpoint::bind("127.0.0.1:0".parse().unwrap(), None).unwrap();
        client_ep.set_default_client_config(client_config);
        let cfg = QuicAdapterConfig::default();
        let expected = message();
        let (conn_client, conn_server) = tokio::join!(
            client_ep.connect(server_addr, "localhost"),
            server.accept()
        );
        // Bind (don't inline `.unwrap()` by reference): dropping the last
        // Connection handle closes the connection, racing the server read.
        let conn_client = conn_client.unwrap();
        let conn_server = conn_server.unwrap();
        send_message(&conn_client, &expected, &cfg).await.unwrap();
        let got = recv_message(&conn_server, &cfg).await.unwrap();
        assert_eq!(expected, got, "PEM-loaded config must serve QUIC");

        assert!(matches!(
            load_server_config_from_pem(&dir.join("missing.pem"), &key_path),
            Err(MqlError::Quic(_))
        ));
        let empty_path = dir.join("empty.pem");
        std::fs::File::create(&empty_path).unwrap();
        assert!(matches!(
            load_server_config_from_pem(&empty_path, &key_path),
            Err(MqlError::Quic(_))
        ));
        assert!(matches!(
            load_server_config_from_pem(&cert_path, &cert_path),
            Err(MqlError::Quic(_))
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
