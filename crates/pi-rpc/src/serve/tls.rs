//! How requests reach a Gentle Shell server (Gentle Shell's
//! `lib/serve-tls.ts` and `lib/serve-relay.ts`): ureq's HTTP over a
//! connector of this client's own.
//!
//! - **Plain TCP** for `http://`: a server without TLS (on this computer,
//!   or one started with `--no-tls`).
//! - **Pinned TLS** for `https://`: the server's certificate is self-signed,
//!   so no CA and no host name says anything; the one check is that its
//!   SHA-256 fingerprint is the one this device paired with
//!   ([`Pin::Exact`]). The handshake's signature is still checked against
//!   that certificate's key. A different certificate ends the connection
//!   before a byte of the request is sent, and the client says so
//!   ([`Link::changed`]). First contact (before a pairing proof) accepts any
//!   certificate and remembers which one it saw ([`Pin::FirstContact`]).
//! - **Through a relay** (`gsr://host:port`, sent to ureq as `https://`
//!   at the relay's address): one handshake line to the relay naming the
//!   server by its fingerprint, then the same pinned TLS end to end, so the
//!   relay sees only ciphertext.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::WebPkiSupportedAlgorithms;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, ClientConnection, DigitallySignedStruct, StreamOwned};
use serde_json::{Value, json};
use ureq::unversioned::transport::{
    Buffers, ConnectionDetails, Connector, LazyBuffers, NextTimeout, TcpConnector, Transport,
    TransportAdapter,
};

use gentle_core::serve as protocol;

/// The relay protocol's version (`RELAY_PROTOCOL`).
const RELAY_VERSION: u64 = 1;
/// The longest handshake line the relay sends, newline included.
const RELAY_LINE: usize = 4096;
/// The name a TLS handshake carries when the host isn't one: the server
/// checks none.
const NO_NAME: &str = "gentle-shell-serve.invalid";

/// Which certificate a connection accepts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Pin {
    /// No TLS: `https://` is refused.
    None,
    /// Exactly the certificate with this fingerprint.
    Exact(String),
    /// Any, remembering it ([`Link::seen`]): only to ask for a pairing
    /// proof, which is then checked against what was seen.
    FirstContact,
}

/// One way of reaching a server, shared by an agent's connections.
#[derive(Debug)]
pub(crate) struct Link {
    pin: Pin,
    /// `host:port` of the relay requests to it go through.
    relay: Option<String>,
    /// Through the relay: the server's port to reach (a forward's
    /// listener); its API without it.
    relay_port: Option<u16>,
    config: Arc<ClientConfig>,
    /// The fingerprint of the last certificate a handshake showed.
    seen: Mutex<Option<String>>,
    /// A server showed a certificate other than the pinned one.
    changed: AtomicBool,
}

impl Link {
    /// A link pinning `pin`, through the relay `relay` (`gsr://host:port`)
    /// when a request goes to it, to the server's `relay_port` there.
    pub(crate) fn new(pin: Pin, relay: Option<&str>, relay_port: Option<u16>) -> Arc<Self> {
        Arc::new_cyclic(|link: &std::sync::Weak<Link>| {
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let verifier = Arc::new(PinVerifier {
                link: link.clone(),
                algorithms: provider.signature_verification_algorithms,
            });
            let config = ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .expect("ring supports the default TLS versions")
                .dangerous()
                .with_custom_certificate_verifier(verifier)
                .with_no_client_auth();
            Link {
                pin,
                relay: relay
                    .and_then(|url| url.strip_prefix(protocol::RELAY_PREFIX))
                    .map(str::to_string),
                relay_port,
                config: Arc::new(config),
                seen: Mutex::new(None),
                changed: AtomicBool::new(false),
            }
        })
    }

    /// The fingerprint of the certificate the last handshake showed.
    pub(crate) fn seen(&self) -> Option<String> {
        self.seen.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Whether a server refused here showed another certificate than the
    /// pinned one.
    pub(crate) fn changed(&self) -> bool {
        self.changed.load(Ordering::Acquire)
    }

    /// The fingerprint pinned, if one is.
    fn fingerprint(&self) -> Option<&str> {
        match &self.pin {
            Pin::Exact(fp) => Some(fp),
            _ => None,
        }
    }
}

/// The URL ureq is given for `base`: a relay's `gsr://host:port` is
/// `https://host:port` (the connector turns it into the relay's handshake).
pub(crate) fn transport_url(base: &str) -> String {
    match base.strip_prefix(protocol::RELAY_PREFIX) {
        Some(authority) => format!("https://{authority}"),
        None => base.to_string(),
    }
}

/// Accepts a server's certificate only as its [`Link`]'s pin says.
#[derive(Debug)]
struct PinVerifier {
    link: std::sync::Weak<Link>,
    algorithms: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for PinVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let refused = || {
            rustls::Error::InvalidCertificate(
                rustls::CertificateError::ApplicationVerificationFailure,
            )
        };
        let link = self.link.upgrade().ok_or_else(refused)?;
        let seen = protocol::certificate_fingerprint(end_entity.as_ref());
        *link.seen.lock().unwrap_or_else(|e| e.into_inner()) = Some(seen.clone());
        match &link.pin {
            Pin::Exact(fp) if *fp == seen => Ok(ServerCertVerified::assertion()),
            Pin::FirstContact => Ok(ServerCertVerified::assertion()),
            Pin::Exact(_) => {
                link.changed.store(true, Ordering::Release);
                Err(refused())
            }
            Pin::None => Err(refused()),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

/// ureq's connector for a [`Link`]: TCP, then the relay's handshake when
/// the request goes to the relay, then pinned TLS for `https://`.
#[derive(Debug)]
pub(crate) struct LinkConnector(pub(crate) Arc<Link>);

impl Connector<()> for LinkConnector {
    type Out = Box<dyn Transport>;

    fn connect(
        &self,
        details: &ConnectionDetails,
        _chained: Option<()>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        let link = &self.0;
        if !details.needs_tls() {
            if link.fingerprint().is_some() {
                return Err(io_error(
                    std::io::ErrorKind::PermissionDenied,
                    "This server speaks HTTPS: its address starts with https://.".into(),
                ));
            }
            let tcp = TcpConnector::default().connect(details, None::<()>)?;
            return Ok(tcp.map(|tcp| -> Box<dyn Transport> { Box::new(tcp) }));
        }
        if link.pin == Pin::None {
            return Err(io_error(
                std::io::ErrorKind::PermissionDenied,
                "This device doesn't know the server's certificate: pair it again.".into(),
            ));
        }
        let Some(tcp) = TcpConnector::default().connect(details, None::<()>)? else {
            return Ok(None);
        };
        let mut sock = TransportAdapter::new(tcp.boxed(), details.current_time.clone());
        sock.set_timeout(details.timeout);
        let authority = details.uri.authority().map(|a| a.as_str());
        if let Some(relay) = link
            .relay
            .as_deref()
            .filter(|relay| authority == Some(*relay))
        {
            let Some(id) = link.fingerprint() else {
                return Err(io_error(
                    std::io::ErrorKind::PermissionDenied,
                    "A relay reaches only a server this device has paired with.".into(),
                ));
            };
            relay_handshake(&mut sock, relay, id, link.relay_port).map_err(ureq::Error::Io)?;
        }
        let host = details
            .uri
            .host()
            .unwrap_or_default()
            .trim_start_matches('[')
            .trim_end_matches(']');
        let name = ServerName::try_from(host.to_string())
            .or_else(|_| ServerName::try_from(NO_NAME.to_string()))
            .map_err(|_| ureq::Error::Tls("no server name"))?;
        let mut conn = ClientConnection::new(link.config.clone(), name)
            .map_err(|e| io_error(std::io::ErrorKind::Other, e.to_string()))?;
        conn.complete_io(&mut sock)?;
        let buffers = LazyBuffers::new(
            details.config.input_buffer_size(),
            details.config.output_buffer_size(),
        );
        Ok(Some(Box::new(PinnedTls {
            buffers,
            stream: StreamOwned { conn, sock },
        })))
    }
}

fn io_error(kind: std::io::ErrorKind, message: String) -> ureq::Error {
    ureq::Error::Io(std::io::Error::new(kind, message))
}

/// The client's side of the relay's handshake on `io`: `{"t":"gsr/client",
/// "v":1,"id":<fingerprint>[,"port"]}` and a newline, then one line back:
/// `{"t":"ok"}` and the stream carries the server's bytes from there, or
/// `{"t":"error","code","message"}`. Only the answer's line is read, so
/// what follows it is left for TLS. A refusal that never reached the server
/// (`offline`, `busy`, `timeout`, `refused`) reads as a refused connection,
/// so the request may be sent elsewhere.
pub(crate) fn relay_handshake(
    io: &mut (impl Read + Write),
    relay: &str,
    id: &str,
    port: Option<u16>,
) -> std::io::Result<()> {
    use std::io::{Error, ErrorKind};
    let mut hello = json!({"t": "gsr/client", "v": RELAY_VERSION, "id": id});
    if let Some(port) = port {
        hello["port"] = port.into();
    }
    io.write_all(format!("{hello}\n").as_bytes())?;
    io.flush()?;
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        if io.read(&mut byte)? == 0 {
            return Err(Error::new(
                ErrorKind::ConnectionAborted,
                format!("The relay {relay} closed the connection without answering."),
            ));
        }
        if byte[0] == b'\n' {
            break;
        }
        line.push(byte[0]);
        if line.len() >= RELAY_LINE {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("{relay} doesn't answer as a Gentle Shell relay."),
            ));
        }
    }
    let answer: Value = serde_json::from_slice(&line).unwrap_or_default();
    if answer["t"] == "ok" {
        return Ok(());
    }
    if answer["t"] != "error" {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!("{relay} doesn't answer as a Gentle Shell relay."),
        ));
    }
    let code = answer["code"].as_str().unwrap_or_default();
    let said = answer["message"].as_str().unwrap_or("it refused").trim();
    let (kind, text) = match code {
        "offline" => (
            ErrorKind::ConnectionRefused,
            "the server isn't connected to it right now".to_string(),
        ),
        "busy" | "timeout" | "refused" => (ErrorKind::ConnectionRefused, said.to_string()),
        "unsupported_version" => (
            ErrorKind::Unsupported,
            format!("{said}: update Gentle Shell GPUI or the relay"),
        ),
        _ => (ErrorKind::PermissionDenied, said.to_string()),
    };
    Err(Error::new(
        kind,
        format!("The relay {relay} answered: {text}."),
    ))
}

/// TLS over a transport, as ureq's own rustls transport does it.
struct PinnedTls {
    buffers: LazyBuffers,
    stream: StreamOwned<ClientConnection, TransportAdapter>,
}

impl Transport for PinnedTls {
    fn buffers(&mut self) -> &mut dyn Buffers {
        &mut self.buffers
    }

    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), ureq::Error> {
        self.stream.get_mut().set_timeout(timeout);
        let output = &self.buffers.output()[..amount];
        self.stream.write_all(output)?;
        Ok(())
    }

    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, ureq::Error> {
        self.stream.get_mut().set_timeout(timeout);
        let input = self.buffers.input_append_buf();
        let amount = self.stream.read(input)?;
        self.buffers.input_appended(amount);
        Ok(amount > 0)
    }

    fn is_open(&mut self) -> bool {
        self.stream.get_mut().get_mut().is_open()
    }

    fn is_tls(&self) -> bool {
        true
    }
}

impl std::fmt::Debug for PinnedTls {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PinnedTls").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stream whose reads come from `input` and whose writes are kept.
    struct Scripted {
        input: std::io::Cursor<Vec<u8>>,
        output: Vec<u8>,
    }

    impl Read for Scripted {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.input.read(buf)
        }
    }

    impl Write for Scripted {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.output.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn scripted(answer: &str) -> Scripted {
        Scripted {
            input: std::io::Cursor::new(answer.as_bytes().to_vec()),
            output: Vec::new(),
        }
    }

    #[test]
    fn the_relay_hears_the_server_and_its_answer_line_alone_is_read() {
        let mut io = scripted("{\"t\":\"ok\"}\n\x16\x03\x03");
        relay_handshake(&mut io, "relay:7700", "FP", None).unwrap();
        assert_eq!(io.output, b"{\"t\":\"gsr/client\",\"v\":1,\"id\":\"FP\"}\n");
        // The server's first TLS bytes stay unread.
        assert_eq!(io.input.position(), 11);

        let mut io = scripted("{\"t\":\"ok\"}\n");
        relay_handshake(&mut io, "relay:7700", "FP", Some(8801)).unwrap();
        let hello: Value = serde_json::from_slice(&io.output).unwrap();
        assert_eq!(hello["port"], 8801);
    }

    #[test]
    fn a_relay_refusal_says_why() {
        let refused = |answer: &str| {
            relay_handshake(&mut scripted(answer), "relay:7700", "FP", None).unwrap_err()
        };
        let offline = refused(
            "{\"t\":\"error\",\"code\":\"offline\",\"message\":\"That server is not connected\"}\n",
        );
        // Never reached the server: it may be looked for elsewhere.
        assert_eq!(offline.kind(), std::io::ErrorKind::ConnectionRefused);
        assert!(
            offline
                .to_string()
                .contains("isn't connected to it right now")
        );
        let wrong = refused("{\"t\":\"error\",\"code\":\"unauthorized\",\"message\":\"No\"}\n");
        assert_eq!(wrong.kind(), std::io::ErrorKind::PermissionDenied);
        assert_eq!(
            refused("HTTP/1.1 400 Bad Request\r\n\r\n\n").kind(),
            std::io::ErrorKind::InvalidData
        );
        assert_eq!(refused("").kind(), std::io::ErrorKind::ConnectionAborted);
        assert_eq!(
            refused(&"x".repeat(RELAY_LINE + 1)).kind(),
            std::io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn a_relay_url_goes_to_ureq_as_https_at_the_relay() {
        assert_eq!(transport_url("gsr://relay:7700"), "https://relay:7700");
        assert_eq!(transport_url("https://pc:8787"), "https://pc:8787");
        let link = Link::new(Pin::Exact("x".into()), Some("gsr://relay:7700"), None);
        assert_eq!(link.relay.as_deref(), Some("relay:7700"));
    }

    fn certificate(name: &str) -> CertificateDer<'static> {
        let path = format!(
            "{}/tests/fixtures/serve-tls/cert-{name}.der",
            env!("CARGO_MANIFEST_DIR")
        );
        CertificateDer::from(std::fs::read(path).unwrap())
    }

    fn verify(link: &Arc<Link>, cert: &CertificateDer<'_>) -> bool {
        let verifier = PinVerifier {
            link: Arc::downgrade(link),
            algorithms: rustls::crypto::ring::default_provider().signature_verification_algorithms,
        };
        let name = ServerName::try_from("192.168.1.20").unwrap();
        verifier
            .verify_server_cert(cert, &[], &name, &[], UnixTime::now())
            .is_ok()
    }

    #[test]
    fn the_verifier_accepts_exactly_the_pinned_certificate() {
        let (a, b) = (certificate("a"), certificate("b"));
        let fp_a = protocol::certificate_fingerprint(a.as_ref());

        let pinned = Link::new(Pin::Exact(fp_a.clone()), None, None);
        // No CA and no host name: the self-signed certificate for another
        // name is accepted for its fingerprint alone.
        assert!(verify(&pinned, &a));
        assert!(!pinned.changed());
        assert!(!verify(&pinned, &b));
        assert!(pinned.changed(), "another certificate is a change");
        assert_eq!(
            pinned.seen(),
            Some(protocol::certificate_fingerprint(b.as_ref()))
        );

        let first = Link::new(Pin::FirstContact, None, None);
        assert!(verify(&first, &b));
        assert_eq!(
            first.seen(),
            Some(protocol::certificate_fingerprint(b.as_ref()))
        );
        assert!(!first.changed());

        let none = Link::new(Pin::None, None, None);
        assert!(!verify(&none, &a));
        assert!(!none.changed());
    }
}
