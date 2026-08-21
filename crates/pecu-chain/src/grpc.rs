//! Native gRPC over HTTP/2 — the protocol lightwalletd actually serves.
//!
//! # Why this exists at all
//!
//! The SDK ships [`GrpcWebTransport`], which speaks **grpc-web over HTTP/1.1**.
//! That is a deliberate choice there: grpc-web needs no HTTP/2 stack and no
//! async runtime, so one transport serves a desktop build and a browser one.
//!
//! No lightwalletd speaks it. lightwalletd listens for **native gRPC over
//! HTTP/2** and has no HTTP/1.1 listener at all, so the SDK's transport can
//! only reach one through a translating proxy. Until this module existed, that
//! proxy had to be somebody's — this wallet shipped an address behind one, and
//! every user's block requests went through the machine of whoever ran it.
//!
//! The symptom, when the proxy is missing, is worth recording because it looks
//! like anything but a protocol mismatch:
//!
//! ```text
//! Header field didn't end with \n: [0, 0, 6, 4, 0, 0, 0, 0, 0, 0, 5, 0, 0, 64, 0]
//! ```
//!
//! That is an HTTP/1.1 client reading an HTTP/2 SETTINGS frame and trying to
//! parse it as a header line.
//!
//! # What is the same, and what is not
//!
//! The *message* framing is identical — a 5-byte prefix of one flag byte and a
//! big-endian length, which is why [`crate::light`] can hand the SDK's already
//! framed request bytes straight to this transport and hand the response body
//! back to the SDK's decoder. Two things differ, and both are handled here:
//!
//! * the content type is `application/grpc`, not `application/grpc-web+proto`;
//! * the gRPC status arrives in HTTP/2 **trailers** rather than as a trailer
//!   *frame* inside the body. It is lifted into [`HttpResponse::status`], which
//!   is the field the SDK's client already reads for the trailers-only error
//!   case, so nothing above this file has to know which dialect answered.
//!
//! # Blocking, on purpose
//!
//! [`LightTransport::call`] is synchronous and this wallet has no async layer
//! for it to live in. So the runtime is owned here: a current-thread reactor,
//! driven only while a call is in flight. Between calls nothing is polled,
//! which for a request/response client costs only that a connection closed
//! while idle is discovered at the next call — where it is retried — rather
//! than at the moment it happens.
//!
//! One connection is kept and reused. HTTP/2 could multiplex several calls
//! over it, but the SDK's client is sequential, so the connection is held
//! behind a mutex for the duration of a call rather than shared. That is not a
//! bottleneck this wallet can reach; it is one fewer thing to reason about.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use tokio::io::{AsyncRead, AsyncWrite};
use verus_sdk::verus_light::grpc::parse_status;
use verus_sdk::verus_light::{HttpResponse, LightError, LightTransport};

/// The content type that selects native gRPC with protobuf payloads.
const CONTENT_TYPE: &str = "application/grpc";

/// Cap on a single response body, mirroring the SDK's grpc-web transport.
///
/// Same reasoning: a block range is the one call that can legitimately return
/// a lot, and 64 MiB covers a very large sweep while still refusing a server
/// that intends to exhaust memory.
const MAX_RESPONSE: usize = 64 * 1024 * 1024;

/// How long to wait for a TCP connection and a TLS handshake.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long one call may take, end to end.
///
/// The same two minutes the SDK's transport allows. A full-chain scan issues
/// many calls rather than one long one, so this bounds a stalled server and
/// not an honest large fetch.
const CALL_TIMEOUT: Duration = Duration::from_mins(2);

/// HTTP/2 flow-control windows.
///
/// The protocol default is 64 KiB, which for a multi-megabyte block range means
/// the server stops every 64 KiB and waits to be told to continue. 1 MiB
/// removes most of those stalls, and bounds how much of a response can sit
/// unconsumed — which is what [`DATA_FRAME_BUDGET`] below is sized against.
/// Capacity is released per chunk as the body is read, so this is a starting
/// window and not a memory commitment.
const WINDOW: u32 = 1024 * 1024;

/// How much DATA-frame *framing overhead* h2 will tolerate before it hangs up.
///
/// # What this is defending against, and why the default is wrong here
///
/// h2 charges every DATA frame smaller than 256 bytes the difference, and
/// refunds it when the frame is consumed. An endless stream of empty DATA
/// frames costs a peer nothing to send and costs the receiver a bookkeeping
/// entry each, so without a cap it is a memory attack — hence the guard.
///
/// Its default budget is 25 600, or a hundred empty frames in flight. That is
/// far below what lightwalletd legitimately does: `GetBlockRange` streams one
/// message per block, an empty testnet block compacts to a few dozen bytes, and
/// the server puts each in its own DATA frame. A thousand-block range therefore
/// arrives as roughly a thousand undersized frames — and faster than any reader
/// drains them, because the connection is read in bursts. The wallet saw it as
///
/// ```text
/// connection error detected: detected excessive load generating behavior
/// ("too_many_data_frames")
/// ```
///
/// mid-scan, on ranges that had nothing wrong with them.
///
/// So the budget is raised to 100 000 frames' worth, and the *real* bound moves
/// to [`WINDOW`]: a server can only get 1 MiB of payload ahead of the reader,
/// which at lightwalletd's frame sizes is well inside this. The guard still
/// fires for what it was written for — zero-length frames consume no window at
/// all, so an endless run of them still ends the connection, now after 100 000
/// of them rather than 100.
const DATA_FRAME_BUDGET: usize = 256 * 100_000;

/// A blocking native-gRPC transport.
///
/// Construct with [`GrpcTransport::new`], which validates the endpoint and
/// connects nothing. The first [`call`](LightTransport::call) opens the
/// connection.
pub struct GrpcTransport {
    /// Where to open a socket: the host as written, for both DNS and SNI.
    host: String,
    port: u16,
    /// `None` means cleartext HTTP/2 with prior knowledge, which
    /// [`Self::new`] permits only to loopback.
    tls: Option<Arc<rustls::ClientConfig>>,
    /// `:scheme` and `:authority`, decided once so every request agrees with
    /// the socket that carries it.
    scheme: http::uri::Scheme,
    authority: http::uri::Authority,
    runtime: tokio::runtime::Runtime,
    /// The pooled connection, absent until the first call and dropped whenever
    /// a call fails on it.
    live: Mutex<Option<h2::client::SendRequest<Bytes>>>,
}

impl GrpcTransport {
    /// Point a transport at a lightwalletd's own gRPC endpoint.
    ///
    /// # Which endpoints are refused, and by whom
    ///
    /// The scheme and userinfo rules are the SDK's, not this file's:
    /// [`GrpcWebTransport::new`] is called for its validation and the result
    /// thrown away. That is deliberate. Those checks are subtle — a leading
    /// `/` once made `https:///user:pass@host` read as having no authority —
    /// and a second hand-written copy here is exactly how the two drift apart.
    /// One validator, both transports.
    ///
    /// What that buys: no `user:password@` on either scheme, and plaintext
    /// `http://` only to loopback. The second is what makes the cleartext path
    /// below safe to offer — a lightwalletd on `127.0.0.1:9067` with no TLS is
    /// the normal way to run one for yourself, and requiring a certificate for
    /// a connection that never leaves the machine would only push people
    /// towards a public server instead.
    ///
    /// # Errors
    ///
    /// [`LightError::Refused`] for an endpoint that fails any of the above, or
    /// one this transport cannot turn into an HTTP/2 authority.
    pub fn new(endpoint: &str) -> Result<Self, LightError> {
        // The SDK's validator, used for its refusals and nothing else.
        verus_sdk::light::GrpcWebTransport::new(endpoint)?;

        let endpoint = endpoint.trim_end_matches('/');
        let uri: http::Uri = endpoint
            .parse()
            .map_err(|e| LightError::Refused(format!("not a usable endpoint: {e}")))?;

        let scheme = uri
            .scheme()
            .cloned()
            .ok_or_else(|| LightError::Refused("endpoint has no scheme".into()))?;
        let authority = uri
            .authority()
            .cloned()
            .ok_or_else(|| LightError::Refused("endpoint names no host".into()))?;

        // Belt and braces where two parsers meet. The SDK's validator has
        // already refused userinfo, bounding the authority its own way; if
        // `http::Uri` finds credentials the validator did not, the two
        // disagree about what this string addresses and the honest answer is
        // to refuse rather than to pick one.
        if authority.as_str().contains('@') {
            return Err(LightError::Refused(
                "endpoint carries user:password@, which this transport never sends".into(),
            ));
        }
        if uri.path() != "" && uri.path() != "/" {
            return Err(LightError::Refused(format!(
                "a native gRPC endpoint is a host, not a path: drop {:?} from the address",
                uri.path()
            )));
        }

        let tls = match scheme.as_str() {
            "https" => Some(tls_config()?),
            // `GrpcWebTransport::new` above has already established that a
            // plaintext endpoint addresses loopback.
            "http" => None,
            other => {
                return Err(LightError::Refused(format!(
                    "endpoint must start with http:// or https://, got scheme {other:?}"
                )))
            }
        };

        let host = authority.host().to_string();
        let port = authority
            .port_u16()
            .unwrap_or(if tls.is_some() { 443 } else { 80 });

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .map_err(|e| LightError::Transport(format!("starting a reactor: {e}")))?;

        Ok(Self {
            host,
            port,
            tls,
            scheme,
            authority,
            runtime,
            live: Mutex::new(None),
        })
    }

    /// Where this is talking to, as it was written.
    pub fn endpoint(&self) -> String {
        format!("{}://{}", self.scheme, self.authority)
    }

    /// Open a connection and hand back the half that sends requests.
    ///
    /// The other half — the connection future that actually moves bytes — is
    /// spawned onto this transport's runtime, where it is polled for as long
    /// as some call is blocked on it.
    async fn connect(&self) -> Result<h2::client::SendRequest<Bytes>, LightError> {
        let tcp = tokio::time::timeout(
            CONNECT_TIMEOUT,
            tokio::net::TcpStream::connect((self.host.as_str(), self.port)),
        )
        .await
        .map_err(|_| {
            LightError::Transport(format!(
                "connecting to {}:{} timed out after {}s",
                self.host,
                self.port,
                CONNECT_TIMEOUT.as_secs()
            ))
        })?
        .map_err(|e| {
            LightError::Transport(format!("connecting to {}:{}: {e}", self.host, self.port))
        })?;
        // Turn off Nagle: every request here is one small frame followed by a
        // wait for the answer, which is the exact shape Nagle delays.
        let _ = tcp.set_nodelay(true);

        match &self.tls {
            Some(config) => {
                let name =
                    rustls_pki_types::ServerName::try_from(self.host.clone()).map_err(|_| {
                        LightError::Refused(format!(
                            "{} is not a valid host name for TLS",
                            self.host
                        ))
                    })?;
                let stream = tokio::time::timeout(
                    CONNECT_TIMEOUT,
                    tokio_rustls::TlsConnector::from(Arc::clone(config)).connect(name, tcp),
                )
                .await
                .map_err(|_| {
                    LightError::Transport(format!(
                        "the TLS handshake with {} did not finish in {}s",
                        self.host,
                        CONNECT_TIMEOUT.as_secs()
                    ))
                })?
                .map_err(|e| {
                    LightError::Transport(format!("TLS handshake with {}: {e}", self.host))
                })?;

                // The one place a wrong-dialect server can be named as such
                // rather than as a framing error fifty lines later. A server
                // that negotiates `http/1.1`, or declines to negotiate at all,
                // is not serving native gRPC — say so, so that the caller
                // trying both dialects has something to report.
                match stream.get_ref().1.alpn_protocol() {
                    Some(b"h2") => {}
                    Some(other) => {
                        return Err(LightError::Transport(format!(
                            "{} negotiated {:?}, not HTTP/2: it is not serving native gRPC",
                            self.host,
                            String::from_utf8_lossy(other)
                        )))
                    }
                    None => {
                        return Err(LightError::Transport(format!(
                            "{} negotiated no protocol over ALPN, so it is not offering HTTP/2",
                            self.host
                        )))
                    }
                }

                self.handshake(stream).await
            }
            None => self.handshake(tcp).await,
        }
    }

    /// The HTTP/2 handshake, once there is a stream to do it on.
    async fn handshake<S>(&self, io: S) -> Result<h2::client::SendRequest<Bytes>, LightError>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let (send, connection) = h2::client::Builder::new()
            .initial_window_size(WINDOW)
            .initial_connection_window_size(WINDOW)
            .data_frame_budget(DATA_FRAME_BUDGET)
            .handshake(io)
            .await
            .map_err(|e| {
                LightError::Transport(format!("HTTP/2 handshake with {}: {e}", self.host))
            })?;

        // The connection future is the I/O loop. Dropping it would close the
        // connection immediately, so it is spawned and left to run; when the
        // peer goes away it resolves and the next call finds a dead
        // `SendRequest` and reconnects. Its error is not logged at the point
        // it happens because nothing is waiting to hear it — the call that
        // trips over the closed connection reports one that is actually
        // attributable to a request.
        self.runtime.spawn(async move {
            let _ = connection.await;
        });

        send.ready().await.map_err(|e| {
            LightError::Transport(format!("HTTP/2 connection to {} not ready: {e}", self.host))
        })
    }

    /// One request on an already-open connection.
    async fn request(
        send: &mut h2::client::SendRequest<Bytes>,
        uri: http::Uri,
        body: Bytes,
    ) -> Result<HttpResponse, LightError> {
        let request = http::Request::builder()
            .method(http::Method::POST)
            .uri(uri)
            .header(http::header::CONTENT_TYPE, CONTENT_TYPE)
            // Required by the gRPC spec, and lightwalletd's own framework
            // rejects a request without it. It is what says "this client
            // understands trailers", which is where the status is coming from.
            .header("te", "trailers")
            .body(())
            .map_err(|e| LightError::Transport(format!("building the request: {e}")))?;

        let mut send = send
            .clone()
            .ready()
            .await
            .map_err(|e| LightError::Transport(format!("stream not ready: {e}")))?;

        let (response, mut stream) = send
            .send_request(request, false)
            .map_err(|e| LightError::Transport(format!("sending the request: {e}")))?;

        // A framed lightwalletd request is a few dozen bytes, so this fits in
        // any window a server could offer and does not need the reserve /
        // poll-capacity dance a large upload would.
        stream
            .send_data(body, true)
            .map_err(|e| LightError::Transport(format!("sending the request body: {e}")))?;

        let response = response
            .await
            .map_err(|e| LightError::Transport(format!("waiting for the response: {e}")))?;

        let (parts, mut body) = response.into_parts();

        if parts.status.is_redirection() {
            return Err(LightError::Transport(format!(
                "server returned a redirect ({}); redirects are refused, call the endpoint directly",
                parts.status
            )));
        }
        if !parts.status.is_success() {
            return Err(LightError::Transport(format!(
                "server returned HTTP {}",
                parts.status
            )));
        }

        // A trailers-only response — the shape of every gRPC error — puts the
        // status in the response headers and sends no body at all. Read it
        // here for the same reason the SDK's grpc-web transport does: missing
        // it turns "that range is past the tip" into "that range held no
        // notes", and a wallet that believes the second scans past its own
        // money.
        let header_status = status_from(&parts.headers);

        let mut collected = Vec::new();
        while let Some(chunk) = body.data().await {
            let chunk = chunk
                .map_err(|e| LightError::Transport(format!("reading the response body: {e}")))?;
            if collected.len() + chunk.len() > MAX_RESPONSE {
                return Err(LightError::Transport(format!(
                    "response exceeds the {MAX_RESPONSE} byte cap; ask for a smaller block range"
                )));
            }
            // Without this the connection window closes after `WINDOW` bytes
            // and the server waits forever for permission to send more, which
            // presents as a scan that hangs rather than as an error.
            let _ = body.flow_control().release_capacity(chunk.len());
            collected.extend_from_slice(&chunk);
        }

        let trailers = body
            .trailers()
            .await
            .map_err(|e| LightError::Transport(format!("reading the trailers: {e}")))?;

        // Headers first: a trailers-only response has already said everything
        // it is going to, and asking for trailers on top would find nothing.
        let status = header_status.or_else(|| trailers.as_ref().and_then(status_from));

        Ok(HttpResponse {
            status,
            body: collected,
        })
    }
}

/// Lift `grpc-status` / `grpc-message` out of an HTTP/2 header block.
///
/// Reuses the SDK's parser rather than reading the two headers directly, so
/// both dialects agree on what a status is — including the case-insensitivity
/// that a proxy capitalising one and not the other made necessary.
fn status_from(headers: &http::HeaderMap) -> Option<verus_sdk::verus_light::GrpcStatus> {
    let code = headers.get("grpc-status")?.to_str().ok()?;
    let message = headers
        .get("grpc-message")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    parse_status(&format!("grpc-status: {code}\ngrpc-message: {message}"))
}

/// The TLS settings every connection from this transport uses.
///
/// # Why the provider is named rather than defaulted
///
/// `rustls` 0.23 can take its crypto provider from a process-global that some
/// other crate may or may not have installed. Reading that global here would
/// make this transport's behaviour depend on link order and on whichever
/// dependency ran first. `ring` is what this workspace already compiles, so it
/// is named.
fn tls_config() -> Result<Arc<rustls::ClientConfig>, LightError> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    let mut config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|e| LightError::Transport(format!("configuring TLS: {e}")))?
    .with_root_certificates(roots)
    .with_no_client_auth();

    // Offer HTTP/2 and nothing else. Offering `http/1.1` alongside it would
    // let a server that serves both pick the one this transport cannot speak,
    // and the failure would arrive as a framing error rather than as a refusal
    // to negotiate.
    config.alpn_protocols = vec![b"h2".to_vec()];

    Ok(Arc::new(config))
}

impl LightTransport for GrpcTransport {
    /// # What a retry can and cannot fix
    ///
    /// A pooled HTTP/2 connection can be closed while idle — by a timeout at
    /// the server, by a GOAWAY, by anything between. Nothing polls it in the
    /// meantime (see the module docs), so that is discovered here, on a
    /// request that never reached the server. Retrying it once on a fresh
    /// connection is safe and invisible.
    ///
    /// A failure on a connection this call *opened* is not retried. There is
    /// nothing stale to blame, so a second attempt would only take twice as
    /// long to report the same thing.
    ///
    /// Note what this means for `SendTransaction`, the one method that is not
    /// a question: a retry after a connection died mid-request could in
    /// principle submit the same transaction twice. That is harmless here —
    /// a duplicate is rejected by the network as already-known — and this
    /// wallet broadcasts through the RPC node rather than the light server
    /// anyway.
    fn call(&self, path: &str, request: &[u8]) -> Result<HttpResponse, LightError> {
        let uri = http::Uri::builder()
            .scheme(self.scheme.clone())
            .authority(self.authority.clone())
            .path_and_query(format!("/{}", path.trim_start_matches('/')))
            .build()
            .map_err(|e| LightError::Transport(format!("building the request path: {e}")))?;
        let body = Bytes::copy_from_slice(request);

        let mut live = self
            .live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        self.runtime.block_on(async {
            // Taken rather than borrowed, so a connection that fails below is
            // simply never put back: the pool holds only connections that have
            // answered.
            let mut reused = live.is_some();
            let mut send = match live.take() {
                Some(send) => send,
                None => self.connect().await?,
            };

            loop {
                let attempt = tokio::time::timeout(
                    CALL_TIMEOUT,
                    Self::request(&mut send, uri.clone(), body.clone()),
                )
                .await
                .unwrap_or_else(|_| {
                    Err(LightError::Transport(format!(
                        "{} did not answer {path} within {}s",
                        self.host,
                        CALL_TIMEOUT.as_secs()
                    )))
                });

                match attempt {
                    Ok(response) => {
                        *live = Some(send);
                        return Ok(response);
                    }
                    Err(error) if !reused => return Err(error),
                    Err(error) => {
                        tracing::debug!(
                            %error,
                            "the pooled HTTP/2 connection was stale; opening a new one"
                        );
                        reused = false;
                        send = self.connect().await?;
                    }
                }
            }
        })
    }
}

impl std::fmt::Debug for GrpcTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GrpcTransport")
            .field("endpoint", &self.endpoint())
            .field("host", &self.host)
            .field("port", &self.port)
            .field("scheme", &self.scheme.as_str())
            .field("authority", &self.authority.as_str())
            // Whether TLS is in use, not how it is configured: the settings are
            // the same for every connection and printing them says nothing
            // about this one.
            .field("tls", &self.tls.is_some())
            .field("live", &self.live.lock().is_ok_and(|held| held.is_some()))
            // Named rather than printed, so `missing_fields_in_debug` still
            // guards the rest of this impl.
            .field("runtime", &"<current-thread reactor>")
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The SDK's refusals apply here too, because they are the SDK's.
    ///
    /// Not a re-test of `check_scheme` — that is tested where it lives. This
    /// pins that the validation is still *wired up*, which is the thing that
    /// can silently stop being true when this constructor is edited.
    #[test]
    fn the_endpoints_the_sdk_refuses_are_refused_here() {
        for endpoint in [
            "https://user:secret@lightwalletd.example",
            "http://lightwalletd.example:9067",
            "lightwalletd.example:9067",
            "ftp://lightwalletd.example",
        ] {
            assert!(
                GrpcTransport::new(endpoint).is_err(),
                "{endpoint} should have been refused"
            );
        }
    }

    /// Plaintext to loopback is the local-lightwalletd case, and must work.
    #[test]
    fn plaintext_to_loopback_is_allowed_and_speaks_cleartext() {
        let transport = GrpcTransport::new("http://127.0.0.1:9067").expect("loopback is allowed");
        assert!(
            transport.tls.is_none(),
            "loopback plaintext must not attempt TLS"
        );
        assert_eq!(transport.port, 9067);
        assert_eq!(transport.endpoint(), "http://127.0.0.1:9067");
    }

    /// A missing port means the scheme's default, not a panic or a zero.
    #[test]
    fn https_without_a_port_means_443() {
        let transport = GrpcTransport::new("https://lightwalletd.example").expect("valid endpoint");
        assert_eq!(transport.port, 443);
        assert!(transport.tls.is_some());
    }

    /// A path is a grpc-web proxy's shape, not lightwalletd's.
    ///
    /// Accepting one would build request paths like `/base//Method`, which the
    /// server answers with a 404 that reads as "the wallet is broken".
    #[test]
    fn an_endpoint_with_a_path_is_refused_rather_than_mangled() {
        let refused = GrpcTransport::new("https://proxy.example/lightwalletd")
            .expect_err("a path is not a native gRPC endpoint");
        assert!(
            refused.to_string().contains("host, not a path"),
            "the message should say what to do: {refused}"
        );
    }

    /// A trailing slash is somebody's habit, not a path.
    #[test]
    fn a_trailing_slash_is_tolerated() {
        let transport = GrpcTransport::new("https://lightwalletd.example:9067/").expect("valid");
        assert_eq!(transport.endpoint(), "https://lightwalletd.example:9067");
    }

    /// `Debug` must not become a way to print an endpoint's credentials — and
    /// since credentials are refused outright, the interesting property is
    /// that it stays legible and names the connection state.
    #[test]
    fn debug_says_where_it_points_and_whether_it_is_connected() {
        let transport = GrpcTransport::new("https://lightwalletd.example:9067").expect("valid");
        let text = format!("{transport:?}");
        assert!(text.contains("lightwalletd.example:9067"), "{text}");
        assert!(text.contains("live: false"), "{text}");
    }

    /// The status parser must read what h2 hands over, headers included.
    #[test]
    fn a_grpc_status_is_lifted_out_of_a_header_block() {
        let mut headers = http::HeaderMap::new();
        headers.insert("grpc-status", http::HeaderValue::from_static("5"));
        headers.insert(
            "grpc-message",
            http::HeaderValue::from_static("block requested is newer than latest block"),
        );

        let status = status_from(&headers).expect("a status is present");
        assert_eq!(status.code, 5);
        assert_eq!(status.message, "block requested is newer than latest block");
    }

    /// Headers with no status must stay `None`, or every successful call would
    /// look like it had reported one.
    #[test]
    fn headers_without_a_status_yield_none() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            "content-type",
            http::HeaderValue::from_static("application/grpc"),
        );
        assert!(status_from(&headers).is_none());
    }

    // ── Against a real HTTP/2 server, on loopback ───────────────────────────
    //
    // `call` is the half of this module that cannot be tested by inspecting a
    // struct: framing, flow control, trailers and h2's own guards only happen
    // when something is actually answering. `h2` is already a dependency and
    // its server side comes with it, so the server is twenty lines here rather
    // than a fixture, a proxy, or a network.

    /// How a scripted server should end the response.
    enum Ending {
        /// Some DATA frames of `payload`, then `grpc-status: 0` in trailers.
        /// What every successful lightwalletd call looks like.
        Frames { count: usize, payload: Vec<u8> },
        /// Headers carrying a status and nothing else. What every gRPC *error*
        /// looks like, and the shape that reads as an empty success to a client
        /// that does not look there.
        TrailersOnly {
            code: &'static str,
            message: &'static str,
        },
    }

    /// Answer requests on loopback until the client goes away.
    ///
    /// Returns the port and a thread that yields how many requests it served —
    /// which is what lets a test assert that two calls shared one connection.
    /// It ends when the client drops its transport and the socket closes, so
    /// there is no shutdown channel: a test that needs one has already outgrown
    /// being a test.
    ///
    /// Exactly **one** TCP connection is accepted, deliberately. A transport
    /// that reconnected per call would find nothing listening for its second
    /// connection rather than being quietly served.
    fn scripted_server(ending: Ending) -> (u16, std::thread::JoinHandle<usize>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().expect("local address").port();

        // A thread of its own, not a task: the client half of this test blocks
        // the test thread inside `call`, so a server sharing that thread's
        // runtime would never be polled. The thread does run a tokio runtime —
        // which is what the lint is asking for — it just needs its own.
        #[allow(clippy::disallowed_methods)]
        let handle = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a runtime for the scripted server");

            runtime.block_on(async move {
                listener
                    .set_nonblocking(true)
                    .expect("non-blocking listener");
                let listener = tokio::net::TcpListener::from_std(listener).expect("adopt listener");
                let (socket, _) = listener.accept().await.expect("a client");
                let mut connection = h2::server::handshake(socket).await.expect("h2 handshake");
                let mut served = 0usize;

                // `accept` is also what drives the connection's I/O, so a
                // response queued below is not written until this is awaited
                // again. Serving until the client hangs up rather than until a
                // quota is met is what flushes the last one — returning early
                // dropped the connection with the response still queued, which
                // the client saw as a broken pipe.
                while let Some(accepted) = connection.accept().await {
                    let (_request, mut respond) = accepted.expect("a request");
                    served += 1;

                    match &ending {
                        Ending::Frames { count, payload } => {
                            let response = http::Response::builder()
                                .status(200)
                                .header(http::header::CONTENT_TYPE, CONTENT_TYPE)
                                .body(())
                                .expect("a response");
                            let mut stream = respond
                                .send_response(response, false)
                                .expect("send headers");
                            for _ in 0..*count {
                                stream
                                    .send_data(Bytes::from(payload.clone()), false)
                                    .expect("send a data frame");
                            }
                            let mut trailers = http::HeaderMap::new();
                            trailers.insert("grpc-status", http::HeaderValue::from_static("0"));
                            stream.send_trailers(trailers).expect("send trailers");
                        }
                        Ending::TrailersOnly { code, message } => {
                            let response = http::Response::builder()
                                .status(200)
                                .header(http::header::CONTENT_TYPE, CONTENT_TYPE)
                                .header("grpc-status", *code)
                                .header("grpc-message", *message)
                                .body(())
                                .expect("a response");
                            respond.send_response(response, true).expect("send headers");
                        }
                    }
                }

                served
            })
        });

        (port, handle)
    }

    /// One grpc message frame, the size an empty compact block compacts to.
    fn message_frame(len: usize) -> Vec<u8> {
        let mut frame = vec![0u8];
        frame.extend_from_slice(&u32::try_from(len).expect("small").to_be_bytes());
        frame.extend(std::iter::repeat_n(0x42, len));
        frame
    }

    /// A thousand undersized DATA frames must arrive, not end the connection.
    ///
    /// This is a regression test with a date attached. `GetBlockRange` streams
    /// one message per block and lightwalletd puts each in its own DATA frame;
    /// an empty testnet block compacts to a few dozen bytes. h2's default
    /// budget tolerates a hundred such frames in flight, so a live scan died
    /// part-way through every thousand-block range with
    ///
    /// ```text
    /// detected excessive load generating behavior ("too_many_data_frames")
    /// ```
    ///
    /// See [`DATA_FRAME_BUDGET`]. Reverting that line makes this test fail,
    /// which is the only reason it is worth having.
    #[test]
    fn a_thousand_small_frames_are_a_block_range_and_not_an_attack() {
        let payload = message_frame(35);
        let (port, server) = scripted_server(Ending::Frames {
            count: 1_000,
            payload: payload.clone(),
        });

        let transport =
            GrpcTransport::new(&format!("http://127.0.0.1:{port}")).expect("loopback endpoint");
        let response = transport
            .call("cash.z.wallet.sdk.rpc.CompactTxStreamer/GetBlockRange", &[])
            .expect("a thousand small frames is an ordinary block range");

        assert_eq!(response.body.len(), payload.len() * 1_000);
        assert_eq!(response.status.expect("trailers carried a status").code, 0);

        drop(transport);
        assert_eq!(server.join().expect("the scripted server finished"), 1);
    }

    /// A gRPC error is headers and no body, and must not read as an empty
    /// success.
    ///
    /// The failure this guards is the expensive one: asking for a range past
    /// the tip returns exactly this shape, and a client that reports "no
    /// messages, no error" makes a wallet scan straight past its own notes and
    /// show a balance of nothing.
    #[test]
    fn a_trailers_only_error_is_read_out_of_the_headers() {
        let (port, server) = scripted_server(Ending::TrailersOnly {
            code: "5",
            message: "block requested is newer than latest block",
        });

        let transport =
            GrpcTransport::new(&format!("http://127.0.0.1:{port}")).expect("loopback endpoint");
        let response = transport
            .call("cash.z.wallet.sdk.rpc.CompactTxStreamer/GetBlockRange", &[])
            .expect("a gRPC error is a transport success");

        assert!(
            response.body.is_empty(),
            "a trailers-only response has no body"
        );
        let status = response.status.expect("the status was in the headers");
        assert_eq!(status.code, 5);
        assert_eq!(status.message, "block requested is newer than latest block");

        drop(transport);
        assert_eq!(server.join().expect("the scripted server finished"), 1);
    }

    /// The second call must go over the connection the first one opened.
    ///
    /// A transport that reconnected per call would still pass every other test
    /// here while adding a TLS handshake to each of the twelve hundred calls a
    /// full-chain scan makes. The scripted server accepts exactly one TCP
    /// connection, so a second one would hang rather than be served — which is
    /// what makes this an assertion and not a hope.
    #[test]
    fn a_second_call_reuses_the_first_calls_connection() {
        let payload = message_frame(35);
        let (port, server) = scripted_server(Ending::Frames {
            count: 2,
            payload: payload.clone(),
        });

        let transport =
            GrpcTransport::new(&format!("http://127.0.0.1:{port}")).expect("loopback endpoint");
        for _ in 0..2 {
            let response = transport
                .call("cash.z.wallet.sdk.rpc.CompactTxStreamer/GetLightdInfo", &[])
                .expect("both calls answered");
            assert_eq!(response.body.len(), payload.len() * 2);
        }

        drop(transport);
        assert_eq!(
            server.join().expect("the scripted server finished"),
            2,
            "both calls should have arrived on the one connection this server accepted"
        );
    }
}
