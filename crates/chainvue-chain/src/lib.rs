//! Talking to a Verus node.
//!
//! # What is here
//!
//! * [`Network`] — which chain, derived from what the node reports and never
//!   from a hostname.
//! * [`Node`] / [`NodeManager`] — endpoints, their health, and backoff.
//! * [`SpendPermit`] — the unforgeable token that gates every broadcast.
//! * [`Chain`] — the live client and, behind the `mock` feature, a scripted
//!   one. Both satisfy the SDK's `ChainReader`, so every flow above this layer
//!   is written once.
//!
//! # Why `Chain` is an enum and not `Box<dyn ChainReader>`
//!
//! The SDK's flows take `&impl ChainReader` — a sized generic. `&dyn
//! ChainReader` does not itself implement `ChainReader`, and the orphan rule
//! forbids adding that impl here, since both the trait and `dyn` are foreign.
//!
//! Implementing a foreign trait for a *local* type is allowed, so `Chain` is an
//! enum that delegates. Every flow — `spendable`, `history`, `prepare_send` —
//! then takes `&chain` unchanged, and mock mode costs zero duplicated flow
//! code.

pub mod network;
pub mod node;
pub mod permit;

pub use network::Network;
pub use node::{backoff, connect, probe, validate_url, Client, Node, NodeManager, NodeStatus};
pub use permit::{SpendPermit, SpendRefused};

use verus_sdk::money::Amount;
use verus_sdk::network::{
    AddressBalance, AddressDelta, AddressUtxo, Broadcaster, ChainInfo, ChainReader,
    ConversionEstimate, CurrencyConverter, CurrencyPolicy, CurrencySummary, IdentityAtAddress,
    IdentityContent, IdentityRecord, MempoolDelta, OfferListing, RpcError,
};

/// A chain to read from: a real node, or a scripted one.
pub enum Chain {
    Live(Client),
    /// Only exists when the `mock` feature is on, so a release build does not
    /// contain this variant at all.
    #[cfg(feature = "mock")]
    Mock(chainvue_mock::MockChain),
}

impl Chain {
    /// A client for a real endpoint.
    pub fn live(url: &str) -> Result<Self, RpcError> {
        Ok(Self::Live(connect(url, node::REQUEST_TIMEOUT)?))
    }

    /// The scripted chain, answering for the addresses this wallet holds.
    ///
    /// # Errors
    ///
    /// If the first address is not one the demo script can pay.
    #[cfg(feature = "mock")]
    pub fn mock(addresses: &[String]) -> Result<Self, RpcError> {
        Ok(Self::Mock(chainvue_mock::MockChain::demo(addresses)?))
    }

    /// Ask this chain what it is, and how long it took to answer.
    ///
    /// The companion to [`probe`], which takes a URL and therefore only ever
    /// speaks to a real node. Health and the chain tip are the same question —
    /// one `chain_info()` yields the network, the height, the sync state and the
    /// version — and the scripted chain has to answer it too, or mock mode shows
    /// an offline node beside a populated dashboard.
    pub fn probe(&self) -> (Result<ChainInfo, RpcError>, std::time::Duration) {
        let started = std::time::Instant::now();
        let result = self.chain_info();
        (result, started.elapsed())
    }

    /// Whether this is the scripted chain — so the UI can say so, loudly and
    /// permanently.
    pub fn is_mock(&self) -> bool {
        match self {
            Self::Live(_) => false,
            #[cfg(feature = "mock")]
            Self::Mock(_) => true,
        }
    }

    /// A broadcaster — reachable **only** with a permit.
    ///
    /// This signature is the whole spending guard. [`SpendPermit`] cannot be
    /// constructed outside [`permit`], and [`NodeManager::spend_permit`] is its
    /// only constructor, so every check it runs is unskippable: without a
    /// permit there is no broadcaster, and there is no other route to one.
    pub fn broadcaster(&self, _permit: &SpendPermit) -> Permitted<'_> {
        Permitted(self)
    }
}

/// The only thing in this application that can send a transaction.
///
/// A newtype rather than `&dyn Broadcaster` for the same reason [`Chain`] is an
/// enum rather than `Box<dyn ChainReader>`: `&dyn Broadcaster` does not itself
/// implement `Broadcaster`, and the SDK's `Unsent::broadcast` takes
/// `&impl Broadcaster`.
///
/// What matters is that it stays unconstructable without a permit.
/// [`Chain::broadcaster`] is its only constructor and the field is private, so
/// implementing `Broadcaster` here does **not** widen the guard — a `Chain`
/// alone still cannot send anything.
pub struct Permitted<'a>(&'a Chain);

impl Broadcaster for Permitted<'_> {
    fn send_raw_transaction(&self, hex: &str) -> Result<String, RpcError> {
        match self.0 {
            Chain::Live(client) => client.send_raw_transaction(hex),
            #[cfg(feature = "mock")]
            Chain::Mock(mock) => mock.send_raw_transaction(hex),
        }
    }
}

/// Forward every `ChainReader` method to whichever backend is in use.
///
/// A macro because the trait has 25 methods and hand-writing them twice is how
/// one quietly ends up behaving differently from the other.
macro_rules! delegate {
    ($($method:ident ( $($arg:ident : $ty:ty),* ) -> $ret:ty;)*) => {
        impl ChainReader for Chain {
            $(
                fn $method(&self $(, $arg: $ty)*) -> Result<$ret, RpcError> {
                    match self {
                        Chain::Live(client) => client.$method($($arg),*),
                        #[cfg(feature = "mock")]
                        Chain::Mock(mock) => mock.$method($($arg),*),
                    }
                }
            )*
        }
    };
}

delegate! {
    chain_info() -> ChainInfo;
    block_count() -> u32;
    best_block_hash() -> String;
    block_hash(height: u32) -> String;
    mempool() -> Vec<String>;
    block(height_or_hash: &str) -> serde_json::Value;
    address_utxos(addresses: &[&str]) -> Vec<AddressUtxo>;
    address_deltas(addresses: &[&str], range: Option<(u32, u32)>) -> Vec<AddressDelta>;
    address_mempool(addresses: &[&str]) -> Vec<MempoolDelta>;
    address_balance(addresses: &[&str]) -> AddressBalance;
    currency(name_or_id: &str) -> CurrencyPolicy;
    currency_definition(name_or_id: &str) -> CurrencySummary;
    estimate_conversion(from: &str, to: &str, amount: &str, via: Option<&str>) -> ConversionEstimate;
    currency_state(name_or_id: &str) -> serde_json::Value;
    list_currencies() -> Vec<CurrencySummary>;
    currency_converters(currencies: &[&str]) -> Vec<CurrencyConverter>;
    estimate_fee(blocks: u32) -> Option<Amount>;
    identity(name_or_id: &str) -> IdentityRecord;
    identities_with_address(address: &str) -> Vec<IdentityAtAddress>;
    identity_at(name_or_id: &str, height: u32) -> IdentityRecord;
    identity_content(name_or_id: &str) -> IdentityContent;
    identity_registration(name_or_id: &str) -> String;
    vdxf_id(name: &str) -> [u8; 20];
    offers(currency_or_id: &str, is_currency: bool, with_tx: bool) -> Vec<OfferListing>;
    verify_message(identity: &str, signature: &str, message: &str) -> bool;
    raw_transaction(txid: &str) -> serde_json::Value;
    decode_raw_transaction(hex: &str) -> serde_json::Value;
    confirmations(txid: &str) -> Option<u32>;
}
