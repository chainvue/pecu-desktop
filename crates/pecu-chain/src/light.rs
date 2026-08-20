//! The lightwalletd server a shielded balance is read from.
//!
//! # Why this is not just another `Node`
//!
//! [`crate::Node`] is a Verus daemon speaking JSON-RPC about transparent
//! addresses. This is a lightwalletd speaking grpc-web about compact blocks,
//! and the two are separate servers reached over separate protocols. A wallet
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
//! # What this deliberately cannot do
//!
//! Spend. The workspace takes the SDK's `light` feature and not `prover`, so
//! `verus_flows::shielded::prepare_spend` does not exist in this build. That is
//! not discipline, it is the dependency graph: the code to build a shielded
//! transaction was never compiled in.

use verus_sdk::light::{GrpcWebTransport, LightClient, LightError};
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

    /// It answered for a different chain.
    #[error("the lightwalletd at {url} serves {reported}, not {expected}")]
    WrongNetwork {
        url: String,
        expected: String,
        reported: String,
    },
}

/// A lightwalletd that has been asked what it is, and answered correctly.
///
/// There is no way to construct one without that check having passed, which is
/// the same shape as [`crate::SpendPermit`]: the guarantee is carried by the
/// type rather than by remembering to call something.
pub struct LightServer {
    client: LightClient<GrpcWebTransport>,
    url: String,
    info: ServerInfo,
}

impl LightServer {
    /// Connect to a named address and refuse it unless it serves `expected`.
    pub fn connect(url: &str, expected: &Network) -> Result<Self, LightRefused> {
        let transport =
            GrpcWebTransport::new(url).map_err(|e| LightRefused::BadUrl(e.to_string()))?;
        let client = LightClient::new(transport);

        let info = client
            .server_info()
            .map_err(|source| LightRefused::Unreachable {
                url: url.to_string(),
                source,
            })?;

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

        Ok(Self {
            client,
            url: url.to_string(),
            info,
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
    pub const fn client(&self) -> &LightClient<GrpcWebTransport> {
        &self.client
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

impl std::fmt::Debug for LightServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LightServer")
            .field("url", &self.url)
            // `info` is summarised rather than dumped: the whole struct in a
            // log line is noise, and the chain name is the field that decides
            // whether this server should have been accepted at all.
            .field("info", &self.info.chain_name)
            // A transport with no state worth printing, named so that
            // `missing_fields_in_debug` still guards the rest.
            .field("client", &"<lightwalletd client>")
            .finish()
    }
}
