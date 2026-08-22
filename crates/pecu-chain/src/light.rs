//! The lightwalletd server a shielded balance is read from.
//!
//! # Why this is not just another `Node`
//!
//! [`crate::Node`] is a Verus daemon speaking JSON-RPC about transparent
//! addresses. This is a lightwalletd streaming compact blocks over gRPC, and
//! the two are separate servers reached over separate protocols. A wallet
//! can have a healthy node and no light server, which is the normal state on
//! every chain but testnet — so they are modelled apart rather than as two
//! flavours of one thing.
//!
//! # The same network guard, for the same reason
//!
//! A node is checked against the network the wallet was told to be on, and is
//! marked `WrongNetwork` the moment it answers with a different chain name.
//! This does exactly that, through the same [`Network::from_chain_name`], and
//! it matters more here rather than less: a transparent balance from the wrong
//! chain is visibly wrong, because the addresses do not match. A shielded
//! balance is a single number with nothing on screen to contradict it.
//!
//! Note what the guard is worth. `chain_name` is a string the server chooses to
//! send. It defeats a misconfiguration — the overwhelmingly likely case — and
//! it does not make a hostile server safe. Nothing here can: lightwalletd's
//! trust model is that the server is trusted for availability and honesty about
//! block contents, which the SDK's `shielded` module states plainly and at
//! length. A balance shown from this path is **a claim by that server**.
//!
//! # Two dialects, one server type
//!
//! A lightwalletd can be reached in two ways, and which one an address wants is
//! not something the person typing it should have to know:
//!
//! * **native gRPC over HTTP/2** — what lightwalletd itself serves, on its own
//!   port, with nothing in front of it. [`crate::grpc::GrpcTransport`].
//! * **grpc-web over HTTP/1.1** — what a translating proxy in front of one
//!   serves. The SDK's [`GrpcWebTransport`].
//!
//! [`LightServer::connect`] tries them in that order and keeps whichever
//! answered. It has to *try*: ALPN cannot tell them apart, because a proxy
//! behind a CDN negotiates HTTP/2 at the edge while still speaking grpc-web
//! underneath. `lwd.chainvue.io` is exactly that shape, so a wallet that
//! decided on the negotiated protocol would pick the wrong dialect for it.
//!
//! The probe is the identifying call this function already had to make, so the
//! cost of the second dialect is one extra round trip on connect, and only for
//! the servers that need it.
//!
//! # What this deliberately cannot do
//!
//! Spend. The workspace takes the SDK's `light` feature and not `prover`, so
//! `verus_flows::shielded::prepare_spend` does not exist in this build. That is
//! not discipline, it is the dependency graph: the code to build a shielded
//! transaction was never compiled in.

use verus_sdk::light::{GrpcWebTransport, LightClient, LightError, LightTransport};
use verus_sdk::verus_light::HttpResponse;

use crate::grpc::GrpcTransport;
// `ServerInfo` is re-exported by `verus-light` but not lifted into the SDK's
// `light` facade, so it is named at its own crate. Same pinned revision — the
// facade re-exports the crate itself for exactly this.
use verus_sdk::verus_light::ServerInfo;

use crate::network::Network;

/// Why a light server was not usable.
#[derive(Debug, thiserror::Error)]
pub enum LightRefused {
    /// This chain has no lightwalletd this wallet knows of.
    ///
    /// Not a failure to reach one — there is no address to try. See
    /// [`Network::light_server`].
    #[error("there is no lightwalletd for {0} that this wallet knows of")]
    NoServer(String),

    /// The address was not one the transport would accept.
    #[error("that is not a usable lightwalletd address: {0}")]
    BadUrl(String),

    /// It did not answer.
    #[error("the lightwalletd at {url} did not answer: {source}")]
    Unreachable {
        url: String,
        #[source]
        source: LightError,
    },

    /// Neither dialect got an answer out of it.
    ///
    /// Both are reported, because they fail for different reasons and only the
    /// pair says what is actually wrong. A certificate that has expired shows
    /// up in both; a wallet pointed at a web server shows up as an HTTP status
    /// in both; a lightwalletd behind a proxy that is down shows a connection
    /// refusal in both. It is when they *differ* that the pair earns its keep —
    /// a native failure of "negotiated http/1.1" beside a grpc-web failure of
    /// "404" says the address is a host that serves something else entirely.
    #[error(
        "the lightwalletd at {url} answered neither dialect — \
         native gRPC: {native}; grpc-web: {web}"
    )]
    NoDialect {
        url: String,
        native: LightError,
        web: LightError,
    },

    /// It answered for a different chain.
    #[error("the lightwalletd at {url} serves {reported}, not {expected}")]
    WrongNetwork {
        url: String,
        expected: String,
        reported: String,
    },
}

/// Whether the transport would accept this address, without connecting.
///
/// The same checks `GrpcWebTransport::new` makes — plaintext only to loopback,
/// no credentials in the address — run at the moment somebody types one rather
/// than at the next scan. A complaint that arrives minutes after the typing
/// that caused it is a complaint about nothing, as far as the person reading it
/// can tell.
///
/// # Errors
///
/// [`LightRefused::BadUrl`] with the transport's own wording.
pub fn validate_light_url(url: &str) -> Result<(), LightRefused> {
    GrpcWebTransport::new(url)
        .map(|_| ())
        .map_err(|e| LightRefused::BadUrl(e.to_string()))
}

/// Which of the two gRPC dialects a server turned out to speak.
///
/// Kept after the probe so it can be reported. "The wallet is talking to your
/// machine through somebody's proxy" and "the wallet is talking to lightwalletd
/// itself" are different situations for anyone thinking about who sees their
/// block requests, and the interface has no way to say so if this is thrown
/// away at connect time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    /// Native gRPC over HTTP/2, straight to lightwalletd.
    Native,
    /// grpc-web over HTTP/1.1, through a proxy in front of one.
    Web,
}

impl Dialect {
    /// A short name for a log line or a settings screen.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Native => "native gRPC",
            Self::Web => "grpc-web",
        }
    }
}

/// Whichever transport the server at hand turned out to want.
///
/// # Why an enum and not a generic
///
/// [`LightServer`] would otherwise have to be generic over its transport, and
/// so would every signature that holds one — `Core`, the scan, the spend
/// preparation. The SDK's flow functions already take `LightClient<T>`
/// generically, so an enum that implements the trait keeps a single concrete
/// `LightServer` type and leaves every one of those call sites untouched.
///
/// The same argument as [`crate::Chain`], one layer up, for the same reason.
pub enum Transport {
    Native(GrpcTransport),
    Web(GrpcWebTransport),
}

impl LightTransport for Transport {
    fn call(&self, path: &str, request: &[u8]) -> Result<HttpResponse, LightError> {
        match self {
            Self::Native(transport) => transport.call(path, request),
            Self::Web(transport) => transport.call(path, request),
        }
    }
}

/// A lightwalletd that has been asked what it is, and answered correctly.
///
/// There is no way to construct one without that check having passed, which is
/// the same shape as [`crate::SpendPermit`]: the guarantee is carried by the
/// type rather than by remembering to call something.
pub struct LightServer {
    client: LightClient<Transport>,
    url: String,
    info: ServerInfo,
    dialect: Dialect,
}

impl LightServer {
    /// Connect to a named address and refuse it unless it serves `expected`.
    ///
    /// Native gRPC is tried first and grpc-web second — see the module docs for
    /// why this has to be a probe rather than a decision. Preferring native is
    /// not a performance choice: it is the form that reaches lightwalletd with
    /// nothing in between, so when both would work, the one with fewer parties
    /// watching wins.
    pub fn connect(url: &str, expected: &Network) -> Result<Self, LightRefused> {
        // `BadUrl` only when *neither* transport would take the address.
        // `GrpcTransport` additionally refuses a path, which is legitimate for
        // grpc-web (a proxy can be mounted under one), so a native refusal on
        // its own means "try the other one", not "bad address".
        let native = GrpcTransport::new(url).map(Transport::Native);
        let web = GrpcWebTransport::new(url).map(Transport::Web);
        if let (Err(_), Err(refused)) = (&native, &web) {
            return Err(LightRefused::BadUrl(refused.to_string()));
        }

        let mut failures: Vec<(Dialect, LightError)> = Vec::new();
        let mut accepted = None;

        for (dialect, transport) in [(Dialect::Native, native), (Dialect::Web, web)] {
            let Ok(transport) = transport else { continue };
            let client = LightClient::new(transport);
            match client.server_info() {
                Ok(info) => {
                    accepted = Some((dialect, client, info));
                    break;
                }
                Err(error) => {
                    tracing::debug!(url, dialect = dialect.label(), %error, "that dialect did not answer");
                    failures.push((dialect, error));
                }
            }
        }

        let Some((dialect, client, info)) = accepted else {
            return Err(refusal(url, failures));
        };

        // The one constructor, so this cannot drift from how a node's network
        // is decided.
        let reported = Network::from_chain_name(&info.chain_name);
        if reported != *expected {
            return Err(LightRefused::WrongNetwork {
                url: url.to_string(),
                expected: expected.chain_name().to_string(),
                reported: reported.chain_name().to_string(),
            });
        }

        tracing::info!(url, dialect = dialect.label(), chain = %info.chain_name, "light server accepted");

        Ok(Self {
            client,
            url: url.to_string(),
            info,
            dialect,
        })
    }

    /// Connect to whatever this chain ships, if it ships one.
    pub fn shipped(network: &Network) -> Result<Self, LightRefused> {
        let url = network
            .light_server()
            .ok_or_else(|| LightRefused::NoServer(network.chain_name().to_string()))?;
        Self::connect(url, network)
    }

    /// The client, for the SDK's scan functions.
    pub const fn client(&self) -> &LightClient<Transport> {
        &self.client
    }

    /// Which dialect this server turned out to speak.
    pub const fn dialect(&self) -> Dialect {
        self.dialect
    }

    /// Where this is talking to.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// What the server said about itself when it was accepted.
    ///
    /// Held from the identifying call rather than re-fetched, so everything
    /// downstream describes the server that passed the check rather than
    /// whatever is answering now.
    pub const fn info(&self) -> &ServerInfo {
        &self.info
    }

    /// The height the server has actually synced to.
    ///
    /// [`ServerInfo::block_height`] is the chain tip; `estimated_height` is how
    /// far the daemon behind it has got. Scanning past the second one asks for
    /// blocks that are not there yet, so this reports the smaller of the two
    /// rather than the more flattering one.
    pub fn synced_height(&self) -> Result<u64, LightError> {
        let latest = self.client.latest_block()?;
        let info = self.client.server_info()?;
        Ok(latest.height.min(if info.estimated_height == 0 {
            latest.height
        } else {
            info.estimated_height
        }))
    }
}

/// Turn what the dialects reported into one refusal.
///
/// Both failing is the interesting case and gets [`LightRefused::NoDialect`],
/// which names both. When only one dialect was ever attempted — because the
/// other would not take the address at all — reporting a pair would invent a
/// failure that never happened, so that collapses to the ordinary
/// [`LightRefused::Unreachable`].
fn refusal(url: &str, mut failures: Vec<(Dialect, LightError)>) -> LightRefused {
    let web = failures
        .iter()
        .position(|(dialect, _)| *dialect == Dialect::Web)
        .map(|at| failures.remove(at).1);
    let native = failures.pop().map(|(_, error)| error);

    match (native, web) {
        (Some(native), Some(web)) => LightRefused::NoDialect {
            url: url.to_string(),
            native,
            web,
        },
        (Some(source), None) | (None, Some(source)) => LightRefused::Unreachable {
            url: url.to_string(),
            source,
        },
        // `connect` refuses an address neither transport accepts before it
        // gets here, so there is always at least one attempt to report.
        (None, None) => LightRefused::BadUrl(url.to_string()),
    }
}

impl std::fmt::Debug for LightServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LightServer")
            .field("url", &self.url)
            // `info` is summarised rather than dumped: the whole struct in a
            // log line is noise, and the chain name is the field that decides
            // whether this server should have been accepted at all.
            .field("info", &self.info.chain_name)
            .field("dialect", &self.dialect)
            // A transport with no state worth printing, named so that
            // `missing_fields_in_debug` still guards the rest.
            .field("client", &"<lightwalletd client>")
            .finish()
    }
}
