//! A scripted Verus chain, for developing and demonstrating the UI with no node.
//!
//! # Why mock mode cannot move real money
//!
//! Three mechanisms, and it is worth being precise about how strong each one
//! actually is, because one of them is weaker than it first looks.
//!
//! **1. `MockChain` holds no client, so it cannot reach a network.** It answers
//! every [`ChainReader`] method from an in-memory script. There is no
//! `HttpTransport` anywhere in this crate, no URL, no socket — reaching the
//! network is not refused, it is unimplemented. This is the guarantee that
//! actually holds, and [`MockChain::send_raw_transaction`] additionally returns
//! an error unconditionally.
//!
//! **2. Mock txids are unmistakable.** They are `mock…`, not 64 hex characters:
//! unparseable as a `Txid`, useless in an explorer, and impossible to reconcile
//! against a real transaction.
//!
//! **3. The `mock` feature is off by default**, so a release build does not
//! contain the `Chain::Mock` variant at all.
//!
//! ## What this crate deliberately does *not* claim
//!
//! An earlier design asserted that `ureq` is absent from this crate's
//! dependency graph, on the strength of taking `verus-rpc` without its `http`
//! feature. **That is not true inside this workspace.** Cargo unifies features
//! across normal dependencies, and `chainvue-chain` needs `verus-rpc/http` for
//! the live client — so `verus-rpc` is compiled with `HttpTransport` present,
//! and `cargo tree -p chainvue-mock` lists `ureq`.
//!
//! The manifest still declines the feature, which is meaningful if this crate is
//! ever built alone, and `tests/no_network_stack.rs` checks the thing that *is*
//! true in every build: no source file here names a transport, a URL or a socket.
//! An assertion over `cargo tree` would have looked stronger and proved nothing.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use verus_rpc::{
    AddressBalance, AddressDelta, AddressUtxo, Broadcaster, ChainInfo, ChainReader,
    ConversionEstimate, CurrencyConverter, CurrencyPolicy, CurrencySummary, IdentityContent,
    IdentityRecord, MempoolDelta, OfferListing, RpcError,
};
use verus_sdk::money::{Amount, Txid, Utxo};

/// The prefix every mock transaction id carries.
///
/// Deliberately not hex-shaped: a real txid is 64 hex characters, so anything
/// starting with this cannot be confused for one, fails to parse as a [`Txid`],
/// and is obviously wrong in a screenshot.
pub const MOCK_TXID_PREFIX: &str = "mock";

/// What the scripted chain will answer with.
#[derive(Clone, Debug)]
pub struct MockState {
    pub chain_name: String,
    pub chain_id: String,
    pub tip: u32,
    /// Per address: the outputs it holds.
    pub utxos: BTreeMap<String, Vec<AddressUtxo>>,
    /// Per address: its movement history, oldest first.
    pub deltas: BTreeMap<String, Vec<AddressDelta>>,
    /// Per address: anything unconfirmed.
    pub mempool: BTreeMap<String, Vec<MempoolDelta>>,
    /// Latency to simulate on every call, so loading states are reachable.
    pub latency: std::time::Duration,
    /// When set, every read fails with this, so error states are reachable too.
    pub fail_reads: Option<String>,
}

impl Default for MockState {
    fn default() -> Self {
        Self {
            chain_name: "VRSCTEST".to_string(),
            chain_id: "iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq".to_string(),
            tip: 1_173_695,
            utxos: BTreeMap::new(),
            deltas: BTreeMap::new(),
            mempool: BTreeMap::new(),
            latency: std::time::Duration::from_millis(140),
            fail_reads: None,
        }
    }
}

/// A chain that answers from a script.
///
/// `Arc<Mutex<_>>` rather than `RefCell`, which makes this `Send + Sync` and
/// therefore usable through the Tokio bridge. The SDK's own
/// `verus_flows::testing::ScriptedReader` is `RefCell`-based and is `Send` but
/// **not** `Sync`, so it cannot go behind an `Arc` in the live task graph —
/// which is why this exists alongside it rather than instead of it. Use
/// `ScriptedReader` for synchronous unit tests of pure functions; use this for
/// anything that runs through the actor.
#[derive(Clone)]
pub struct MockChain {
    state: Arc<Mutex<MockState>>,
}

impl MockChain {
    pub fn new(state: MockState) -> Self {
        Self {
            state: Arc::new(Mutex::new(state)),
        }
    }

    /// A wallet with one funded address, for the demo build.
    ///
    /// # Errors
    ///
    /// Only if the hardcoded fixture txid stops being valid hex, which would be
    /// an edit to this function.
    pub fn funded(address: &str, satoshis: u64) -> Result<Self, RpcError> {
        const FIXTURE_TXID: &str =
            "9f2c1b7e4a6d3f8c05e1b9a7d4c2b0e8b6a4c2d0e8f6a4c2d0e8f6a4c2d0e8f6";

        let mut state = MockState::default();
        let txid = Txid::from_display_hex(FIXTURE_TXID)
            .map_err(|e| RpcError::Unexpected(format!("mock fixture txid is not valid: {e}")))?;
        let utxo = AddressUtxo {
            utxo: Utxo {
                txid,
                vout: 0,
                satoshis: Amount::from_sat(satoshis),
                // A real P2PKH script for this address would need `verus-keys`;
                // the demo build never signs, so an empty script is honest
                // about the fact that nothing here is spendable.
                script_pubkey: Vec::new(),
            },
            address: address.to_string(),
            height: state.tip.saturating_sub(500),
            is_spendable: true,
        };
        state.utxos.insert(address.to_string(), vec![utxo]);
        Ok(Self::new(state))
    }

    /// Read the script, applying the configured latency first so that loading
    /// states in the UI are actually reachable.
    fn read<T>(&self, f: impl FnOnce(&MockState) -> Result<T, RpcError>) -> Result<T, RpcError> {
        let state = self
            .state
            .lock()
            .map_err(|_| RpcError::Unexpected("mock state poisoned".into()))?;
        if state.latency > std::time::Duration::ZERO {
            std::thread::sleep(state.latency);
        }
        if let Some(reason) = &state.fail_reads {
            return Err(RpcError::Transport(reason.clone()));
        }
        f(&state)
    }
}

impl Broadcaster for MockChain {
    /// Always fails, unconditionally.
    ///
    /// Mock mode exists to develop the UI, and a mock that quietly "succeeded"
    /// would let the send flow reach a success screen that never corresponds to
    /// anything — which is precisely the state in which someone ships a bug
    /// that only shows up against a real node. Failing here keeps mock mode
    /// honest about what it is.
    ///
    /// The `Chain` wrapper turns this into a synthetic `mock…` id where the
    /// demo needs one, so the flow is still exercisable end to end.
    fn send_raw_transaction(&self, _hex: &str) -> Result<String, RpcError> {
        Err(RpcError::Unexpected(
            "mock mode cannot broadcast: this build is running against a scripted chain".into(),
        ))
    }
}

impl ChainReader for MockChain {
    fn chain_info(&self) -> Result<ChainInfo, RpcError> {
        self.read(|s| {
            Ok(ChainInfo {
                name: s.chain_name.clone(),
                chain_id: s.chain_id.clone(),
                blocks: s.tip,
                longest_chain: s.tip,
                version: "mock".to_string(),
            })
        })
    }

    fn block_count(&self) -> Result<u32, RpcError> {
        self.read(|s| Ok(s.tip))
    }

    fn best_block_hash(&self) -> Result<String, RpcError> {
        self.read(|s| Ok(format!("{:064x}", s.tip)))
    }

    fn block_hash(&self, height: u32) -> Result<String, RpcError> {
        self.read(|_| Ok(format!("{height:064x}")))
    }

    fn mempool(&self) -> Result<Vec<String>, RpcError> {
        self.read(|_| Ok(Vec::new()))
    }

    fn block(&self, _height_or_hash: &str) -> Result<serde_json::Value, RpcError> {
        self.read(|s| Ok(serde_json::json!({ "height": s.tip })))
    }

    fn address_utxos(&self, addresses: &[&str]) -> Result<Vec<AddressUtxo>, RpcError> {
        self.read(|s| {
            Ok(addresses
                .iter()
                .filter_map(|a| s.utxos.get(*a))
                .flat_map(|v| v.iter().cloned())
                .collect())
        })
    }

    fn address_deltas(
        &self,
        addresses: &[&str],
        _range: Option<(u32, u32)>,
    ) -> Result<Vec<AddressDelta>, RpcError> {
        self.read(|s| {
            Ok(addresses
                .iter()
                .filter_map(|a| s.deltas.get(*a))
                .flat_map(|v| v.iter().cloned())
                .collect())
        })
    }

    fn address_mempool(&self, addresses: &[&str]) -> Result<Vec<MempoolDelta>, RpcError> {
        self.read(|s| {
            Ok(addresses
                .iter()
                .filter_map(|a| s.mempool.get(*a))
                .flat_map(|v| v.iter().cloned())
                .collect())
        })
    }

    fn address_balance(&self, addresses: &[&str]) -> Result<AddressBalance, RpcError> {
        self.read(|s| {
            let total: u64 = addresses
                .iter()
                .filter_map(|a| s.utxos.get(*a))
                .flat_map(|v| v.iter())
                .map(|u| u.utxo.satoshis.to_sat())
                .sum();
            Ok(AddressBalance {
                balance: Amount::from_sat(total),
                received: Amount::from_sat(total),
                currency_balance: BTreeMap::new(),
            })
        })
    }

    fn currency(&self, _name_or_id: &str) -> Result<CurrencyPolicy, RpcError> {
        Err(unsupported("getcurrency"))
    }

    fn currency_definition(&self, _name_or_id: &str) -> Result<CurrencySummary, RpcError> {
        Err(unsupported("getcurrency"))
    }

    fn estimate_conversion(
        &self,
        _from: &str,
        _to: &str,
        _amount: &str,
        _via: Option<&str>,
    ) -> Result<ConversionEstimate, RpcError> {
        Err(unsupported("estimateconversion"))
    }

    fn currency_state(&self, _name_or_id: &str) -> Result<serde_json::Value, RpcError> {
        Err(unsupported("getcurrencystate"))
    }

    fn list_currencies(&self) -> Result<Vec<CurrencySummary>, RpcError> {
        self.read(|_| Ok(Vec::new()))
    }

    fn currency_converters(
        &self,
        _currencies: &[&str],
    ) -> Result<Vec<CurrencyConverter>, RpcError> {
        self.read(|_| Ok(Vec::new()))
    }

    fn estimate_fee(&self, _blocks: u32) -> Result<Option<Amount>, RpcError> {
        self.read(|_| Ok(Some(Amount::from_sat(10_000))))
    }

    fn identity(&self, _name_or_id: &str) -> Result<IdentityRecord, RpcError> {
        Err(unsupported("getidentity"))
    }

    fn identity_at(&self, _name_or_id: &str, _height: u32) -> Result<IdentityRecord, RpcError> {
        Err(unsupported("getidentity"))
    }

    fn identity_content(&self, _name_or_id: &str) -> Result<IdentityContent, RpcError> {
        Err(unsupported("getidentitycontent"))
    }

    fn identity_registration(&self, _name_or_id: &str) -> Result<String, RpcError> {
        Err(unsupported("getidentityhistory"))
    }

    fn vdxf_id(&self, _name: &str) -> Result<[u8; 20], RpcError> {
        Err(unsupported("getvdxfid"))
    }

    fn offers(
        &self,
        _currency_or_id: &str,
        _is_currency: bool,
        _with_tx: bool,
    ) -> Result<Vec<OfferListing>, RpcError> {
        self.read(|_| Ok(Vec::new()))
    }

    fn verify_message(
        &self,
        _identity: &str,
        _signature: &str,
        _message: &str,
    ) -> Result<bool, RpcError> {
        Err(unsupported("verifymessage"))
    }

    fn raw_transaction(&self, txid: &str) -> Result<serde_json::Value, RpcError> {
        self.read(|_| Ok(serde_json::json!({ "txid": txid, "mock": true })))
    }

    fn decode_raw_transaction(&self, _hex: &str) -> Result<serde_json::Value, RpcError> {
        Err(unsupported("decoderawtransaction"))
    }

    fn confirmations(&self, _txid: &str) -> Result<Option<u32>, RpcError> {
        self.read(|_| Ok(Some(1)))
    }
}

/// A method the scripted chain does not answer.
///
/// Reported as `MethodUnavailable` rather than a made-up success, so the UI
/// exercises the same path it would take against a public node behind a method
/// allowlist — which is a real thing that happens.
fn unsupported(method: &'static str) -> RpcError {
    RpcError::MethodUnavailable { method }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The guarantee that actually holds, asserted rather than described.
    #[test]
    fn broadcasting_always_fails() {
        let chain = MockChain::new(MockState::default());
        let result = chain.send_raw_transaction("0400008085202f8900000000000000000000");
        assert!(
            result.is_err(),
            "mock mode broadcast a transaction; it must never succeed"
        );
    }

    /// No simulated latency in tests — the delay exists so the UI's loading
    /// states are reachable by hand, and it would only make the suite slow.
    fn instant() -> MockState {
        MockState {
            latency: std::time::Duration::ZERO,
            ..MockState::default()
        }
    }

    #[test]
    fn reads_answer_from_the_script() {
        let chain = MockChain::new(MockState {
            tip: 42,
            ..instant()
        });
        assert_eq!(chain.block_count().expect("tip"), 42);
        assert_eq!(chain.chain_info().expect("info").name, "VRSCTEST");
    }

    /// Error states have to be reachable, or they never get designed.
    #[test]
    fn a_scripted_failure_surfaces_as_a_transport_error() {
        let chain = MockChain::new(MockState {
            fail_reads: Some("scripted outage".to_string()),
            ..instant()
        });
        assert!(matches!(
            chain.block_count(),
            Err(RpcError::Transport(reason)) if reason == "scripted outage"
        ));
    }
}
