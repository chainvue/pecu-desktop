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
//! **2. The `mock` feature is off by default**, so a release build does not
//! contain the `Chain::Mock` variant at all — and the demo build keeps its
//! wallet, settings and caches in a directory of their own, so a scripted figure
//! can never be read back by a real run.
//!
//! **3. A scripted send cannot reach a success screen.** Not by convention: the
//! SDK compares the id a node reports against the one computed while signing and
//! refuses a mismatch, so the only id this crate could return and have accepted
//! is the real one. See [`MockChain::send_raw_transaction`].
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

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use verus_rpc::{
    AddressBalance, AddressDelta, AddressUtxo, Broadcaster, ChainInfo, ChainReader,
    ConversionEstimate, CurrencyConverter, CurrencyPolicy, CurrencySummary, IdentityContent,
    IdentityRecord, MempoolDelta, OfferListing, RpcError, SignedAmount,
};
use verus_sdk::identity::{FLAG_LOCKED, FLAG_REVOKED};
use verus_sdk::money::{Amount, Txid, Utxo};
use verus_sdk::verus_keys::Address;

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
    /// Transactions [`ChainReader::raw_transaction`] should describe as a
    /// coinbase, by display hex.
    ///
    /// This exists so mined-but-immature coins are reachable in the demo. The
    /// SDK decides maturity by asking `getrawtransaction` whether an output's
    /// transaction has a `coinbase` input, so a script that never says yes can
    /// only ever produce a dashboard where spendable equals the balance — and
    /// the whole argument for showing three numbers instead of one is the case
    /// where they differ.
    pub coinbase: BTreeSet<String>,
    /// VerusIDs this chain knows, keyed by the name somebody would type.
    ///
    /// Paying by name is the one destination a wallet cannot check offline: a
    /// name is a question put to a node, and everything downstream depends on
    /// the answer. A scripted chain that refuses the question leaves that whole
    /// path unreachable without a real node.
    pub identities: BTreeMap<String, IdentityRecord>,
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
            coinbase: BTreeSet::new(),
            identities: BTreeMap::new(),
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

    /// The wallet the demo build shows: a month of activity at the first
    /// address, and nothing at the others.
    ///
    /// # What the script is chosen to make reachable
    ///
    /// Not "some money" — the specific states a screen is wrong about until
    /// somebody has seen them:
    ///
    /// * **Three different balances.** 527.75 confirmed, of which 415.25 is
    ///   spendable and 112.50 is mined coin still maturing, with 5.00 more
    ///   arriving unconfirmed. A dashboard that renders one number cannot be
    ///   told apart from a correct one until those figures differ.
    /// * **A chart with shape.** Five movements over roughly a month, in both
    ///   directions, so the staircase has steps rather than being a flat line
    ///   with a dot on it.
    /// * **A second address holding nothing**, because a wallet where every key
    ///   has money never shows the empty row.
    ///
    /// The deltas sum to exactly what the outputs hold, so the chart's anchor
    /// and the balance agree. They are meant to: the chart is built backwards
    /// from the confirmed balance, and a script where the two disagree would
    /// have the demo quietly exercising a case that cannot occur.
    ///
    /// # Why the timestamps are relative to now
    ///
    /// Block times are computed backwards from the current clock at one minute
    /// per block, so the newest movement is always "a few minutes ago". A frozen
    /// timestamp would be more reproducible and would also mean the demo reads
    /// as an abandoned wallet a month after this was written — and "how long
    /// ago" is exactly what the activity list is for. The *content* is fixed;
    /// only its distance from today moves.
    ///
    /// # Errors
    ///
    /// If the first address is not a transparent pay-to-public-key-hash
    /// address, since there is then no output script to pay it at.
    pub fn demo(addresses: &[String]) -> Result<Self, RpcError> {
        /// What moved, and when: blocks before the tip, satoshis, and whether it
        /// was mined rather than received.
        const MOVEMENTS: &[(u32, i64, bool)] = &[
            (41_000, 25_000_000_000, false), //  +250.00, about a month ago
            (18_000, 7_550_000_000, false),  //   +75.50
            (9_600, -3_025_000_000, false),  //   -30.25, a payment out
            (2_400, 12_000_000_000, false),  //  +120.00
            (80, 11_250_000_000, true),      //  +112.50, mined, still maturing
        ];
        /// What is left over, and which movement produced it: satoshis, blocks
        /// before the tip, movement index.
        ///
        /// Two outputs rather than five, because change consolidates — and
        /// because the mined one has to stand alone for the balance to split
        /// three ways at all.
        const OUTPUTS: &[(u64, u32, usize)] = &[
            // What the four confirmed, received movements come to, summed.
            (41_525_000_000, 2_400, 3),
            // The mined one: eighty blocks old, and therefore twenty short of
            // the hundred it needs.
            (11_250_000_000, 80, 4),
        ];
        /// Money on its way, which neither the outputs nor the deltas know
        /// about. Without it the "pending" figure is unreachable.
        const INCOMING: i64 = 500_000_000; // +5.00

        let mut state = MockState::default();
        let Some(address) = addresses.first() else {
            return Ok(Self::new(state));
        };

        let tip = state.tip;
        let script = address
            .parse::<Address>()
            .and_then(|parsed| parsed.p2pkh_script_pubkey())
            .map_err(|error| {
                RpcError::Unexpected(format!("the demo script cannot pay {address}: {error}"))
            })?;

        let mut deltas = Vec::with_capacity(MOVEMENTS.len());
        for (index, (ago, satoshis, mined)) in MOVEMENTS.iter().enumerate() {
            let height = tip.saturating_sub(*ago);
            let txid = fixture_txid(index);
            if *mined {
                state.coinbase.insert(txid.to_display_hex());
            }
            deltas.push(AddressDelta {
                address: address.clone(),
                txid,
                height,
                block_time: clock_at(tip, height),
                block_index: 1,
                index: 0,
                satoshis: SignedAmount::from_sat(*satoshis),
                currency_values: BTreeMap::new(),
                spending: *satoshis < 0,
            });
        }

        let utxos = OUTPUTS
            .iter()
            .map(|(satoshis, ago, movement)| AddressUtxo {
                utxo: Utxo {
                    txid: fixture_txid(*movement),
                    vout: 0,
                    satoshis: Amount::from_sat(*satoshis),
                    script_pubkey: script.clone(),
                },
                address: address.clone(),
                height: tip.saturating_sub(*ago),
                // The node's own opinion, and `true` even for the mined output —
                // which is what a daemon actually reports. Maturity is applied
                // on top of this field rather than through it, so writing
                // `false` would withhold the coin for the wrong reason and hide
                // whether the maturity rule works at all.
                is_spendable: true,
            })
            .collect();

        let incoming = MempoolDelta {
            address: address.clone(),
            txid: fixture_txid(MOVEMENTS.len()),
            index: 0,
            satoshis: SignedAmount::from_sat(INCOMING),
            currency_values: BTreeMap::new(),
            spending: false,
            spends: None,
            timestamp: clock_at(tip, tip),
        };

        seed_identities(&mut state, address);

        state.utxos.insert(address.clone(), utxos);
        state.deltas.insert(address.clone(), deltas);
        state.mempool.insert(address.clone(), vec![incoming]);
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
    /// anything — which is precisely the state in which someone ships a bug that
    /// only shows up against a real node.
    ///
    /// # Why there is no synthetic `mock…` id here either
    ///
    /// An earlier design had this hand back an unmistakable fake id so the demo
    /// could reach its success screen. **The SDK will not accept one**, and it is
    /// right not to: `verus_flows::broadcast` compares whatever the node reports
    /// against the txid computed while signing, and refuses a mismatch, because
    /// continuing would hand the caller an id that tracks a different
    /// transaction. So the only id this could return and have accepted is the
    /// *real* one — which would make a scripted send indistinguishable from a
    /// broadcast that happened. Failing is not a limitation here; it is the only
    /// answer that stays honest.
    ///
    /// Everything up to this point does work: the demo build selects coins,
    /// builds, signs and decodes a real transaction, so the review screen — the
    /// one worth getting right — is fully reachable. What it cannot do is claim
    /// the transaction went anywhere.
    ///
    /// # Why the shape of the failure matters as much as the failure
    ///
    /// It has to be a **refusal**, not an unknown outcome. `verus_flows`
    /// classifies a broadcast error into one of two worlds, and everything it
    /// does not recognise falls into the dangerous one: a dropped connection or
    /// a proxy's HTML becomes `BroadcastUncertain`, because the transaction may
    /// genuinely be on its way.
    ///
    /// The first version of this returned `RpcError::Unexpected`, which lands
    /// there. So the demo build wrote a row to the pending-broadcast ledger,
    /// started polling a chain that had never heard of the transaction, and
    /// would eventually have offered to **send it again** — teaching the one
    /// habit the whole uncertain-broadcast protocol exists to prevent.
    ///
    /// A `-26` is what a daemon says when it has looked at a transaction and
    /// said no. It is also exactly true here: nothing was spent, nothing is in
    /// flight, and there is nothing to resolve.
    fn send_raw_transaction(&self, _hex: &str) -> Result<String, RpcError> {
        Err(RpcError::Node {
            code: -26,
            message: "this build is running against a scripted chain, which has \
                      no network to relay a transaction to"
                .into(),
        })
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

    /// Honours `range`, because the caller pages with it.
    ///
    /// The activity list walks backwards in widening height windows and decides
    /// whether there is anything older from what each window returns. A script
    /// that ignored the bounds would answer every window with the whole history,
    /// and "Load older" would then be exercising a case the real chain never
    /// produces.
    fn address_deltas(
        &self,
        addresses: &[&str],
        range: Option<(u32, u32)>,
    ) -> Result<Vec<AddressDelta>, RpcError> {
        self.read(|s| {
            Ok(addresses
                .iter()
                .filter_map(|a| s.deltas.get(*a))
                .flat_map(|v| v.iter())
                .filter(|delta| {
                    range.is_none_or(|(start, end)| delta.height >= start && delta.height <= end)
                })
                .cloned()
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

    /// The chain's own policy, which is where a registration fee comes from.
    ///
    /// Scripted rather than refused, because refusing it makes claiming a name
    /// untestable without a node — and a path that can only be exercised with
    /// real money is a path nobody exercises.
    fn currency(&self, _name_or_id: &str) -> Result<CurrencyPolicy, RpcError> {
        self.read(|s| {
            Ok(CurrencyPolicy {
                currency_id: s.chain_id.clone(),
                name: s.chain_name.clone(),
                // VRSCTEST's real figure when this was written.
                id_registration_fee: Amount::from_sat(100 * 100_000_000),
                id_referral_levels: 3,
                id_import_fee: Amount::ZERO,
                currency_registration_fee: Amount::ZERO,
                proof_protocol: 1,
            })
        })
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

    /// Answers for scripted names, and refuses the rest the way a daemon does.
    ///
    /// `-5` is what a node returns for an identity it does not have, and the
    /// send form distinguishes "no VerusID by that name" from "the node would
    /// not answer" — so returning `MethodUnavailable` here would exercise the
    /// wrong message for the common case of a typo.
    fn identity(&self, name_or_id: &str) -> Result<IdentityRecord, RpcError> {
        self.read(|s| {
            s.identities
                .get(name_or_id)
                .cloned()
                // Also findable by i-address, which is what anything
                // destructive should be using — a scripted chain answering only
                // to names would leave that path untested.
                .or_else(|| {
                    s.identities
                        .values()
                        .find(|record| record.identity_address == name_or_id)
                        .cloned()
                })
                .ok_or_else(|| RpcError::Node {
                    code: -5,
                    message: format!(
                        "identity, i-address or friendly name not found: {name_or_id}"
                    ),
                })
        })
    }

    /// Every scripted identity listing `address` among its primary addresses.
    ///
    /// Answered from the same map `identity` uses rather than from a second
    /// list, so no script can describe a chain where an identity is findable
    /// one way and absent the other.
    fn identities_with_address(
        &self,
        address: &str,
    ) -> Result<Vec<verus_rpc::IdentityAtAddress>, RpcError> {
        self.read(|s| {
            Ok(s.identities
                .values()
                .filter(|record| {
                    record.identity["primaryaddresses"]
                        .as_array()
                        .is_some_and(|all| all.iter().any(|one| one.as_str() == Some(address)))
                })
                .map(|record| verus_rpc::IdentityAtAddress {
                    identity_address: record.identity_address.clone(),
                    name: text_field(&record.identity, "name"),
                    parent: text_field(&record.identity, "parent"),
                    flags: number_field(&record.identity, "flags"),
                    timelock: number_field(&record.identity, "timelock"),
                    outpoint: record.outpoint,
                    identity: record.identity.clone(),
                })
                .collect())
        })
    }

    /// The identity as it stands, whatever height is asked about.
    ///
    /// A scripted chain has no history, and saying so by refusing would send a
    /// caller down the "this node will not answer" path for a question it can
    /// answer perfectly well.
    fn identity_at(&self, name_or_id: &str, _height: u32) -> Result<IdentityRecord, RpcError> {
        self.identity(name_or_id)
    }

    /// Everything the identity has ever published.
    ///
    /// Scripted with **one more value than `getidentity` reports**, because the
    /// two answer different questions and a double that returned the same thing
    /// for both would make them indistinguishable — which is exactly the
    /// confusion the content viewer's two tabs exist to undo.
    fn identity_content(&self, name_or_id: &str) -> Result<IdentityContent, RpcError> {
        let identity = self.identity(name_or_id)?;
        let mut content_multimap = verus_rpc::content_multimap(&identity.identity)?;
        if let Some(values) = content_multimap.get_mut("iGRp1CGkuro3LtGazX8W1PRjVupPVfe8Pv") {
            // An older value under a key that was written twice. Oldest first,
            // as the daemon accumulates them.
            values.insert(
                0,
                verus_rpc::ContentValue::Bytes(b"an older value, since replaced".to_vec()),
            );
        }
        Ok(IdentityContent {
            identity,
            content_map: BTreeMap::new(),
            content_multimap,
        })
    }

    fn identity_registration(&self, name_or_id: &str) -> Result<String, RpcError> {
        let identity = self.identity(name_or_id)?;
        Ok(identity.outpoint.0.to_display_hex())
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

    /// Enough shape for the SDK's maturity check, and honest about the rest.
    ///
    /// `spendable` reads exactly one thing here: whether the first input has a
    /// `coinbase` field. Answering that is what makes mined-but-immature coins
    /// reachable in the demo, so the dashboard's three numbers can differ.
    fn raw_transaction(&self, txid: &str) -> Result<serde_json::Value, RpcError> {
        self.read(|s| {
            let vin = if s.coinbase.contains(txid) {
                serde_json::json!([{ "coinbase": "03deadbeef" }])
            } else {
                serde_json::json!([{ "txid": txid, "vout": 0 }])
            };
            Ok(serde_json::json!({ "txid": txid, "vin": vin, "mock": true }))
        })
    }

    fn decode_raw_transaction(&self, _hex: &str) -> Result<serde_json::Value, RpcError> {
        Err(unsupported("decoderawtransaction"))
    }

    fn confirmations(&self, _txid: &str) -> Result<Option<u32>, RpcError> {
        self.read(|_| Ok(Some(1)))
    }
}

/// A distinct, well-formed transaction id for movement `index`.
///
/// A real 32-byte id rather than a `mock…` string, because these name things the
/// script says are **already on the chain**: the SDK parses them into a [`Txid`]
/// long before the UI sees one, and an unparseable string would fail there
/// rather than anywhere a person could learn something from. The unmistakable
/// prefix belongs on ids this build *produces*, which is a different question
/// with a different answer.
fn fixture_txid(index: usize) -> Txid {
    let mut bytes = [0u8; 32];
    let seed = u8::try_from(index).unwrap_or(0xff);
    for (position, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::try_from(position).unwrap_or(0) ^ seed ^ 0x5a;
    }
    Txid::from_internal(bytes)
}

/// A scripted VerusID.
///
/// The i-addresses are real ones from `api.verustest.net`, so anything that
/// parses them gets a valid address rather than a hand-typed string with a
/// checksum somebody has to keep correct by hand.
/// Seed the VerusIDs this chain knows about.
///
/// Three that `address` controls, in three different states, and one it does
/// not. The states matter: a demo where everything is Active never shows the
/// screens that stop a mistake, and a demo with nobody else's identity never
/// shows the section that keeps a lookup from being counted as yours.
fn seed_identities(state: &mut MockState, address: &str) {
    // Three identities, all controlled by this wallet's own address, chosen
    // so the list shows three different states rather than three rows of
    // the same one. A demo where everything is Active never shows the
    // screens that stop a mistake.
    //
    // `demo@` is ordinary and payable. `vault@` is locked with a delay,
    // which is the state whose wording is easiest to get wrong. `gone@` is
    // revoked, which the send form has to refuse.
    for (typed, name, id_address, flags, timelock) in [
        (
            "demo@",
            "demo.VRSCTEST@",
            "iGRp1CGkuro3LtGazX8W1PRjVupPVfe8Pv",
            0,
            0,
        ),
        (
            "vault@",
            "vault.VRSCTEST@",
            "i5Qcj82gvrHdHCCvTwy2yCFeMz3s3dgB6m",
            FLAG_LOCKED,
            100,
        ),
        (
            "gone@",
            "gone.VRSCTEST@",
            "i87QZVSS7SosM5choTJE7Dy4SNRt5vAEhr",
            FLAG_REVOKED,
            0,
        ),
    ] {
        state.identities.insert(
            typed.to_string(),
            identity(name, id_address, address, flags, timelock),
        );
    }

    // And one that is nobody's here. Somebody else's primary address, so it
    // can be looked up and is not found by the address-scoped search — the
    // case that produced the bug where a stranger's identity sat under
    // "found by asking which names your keys control" until a restart.
    state.identities.insert(
        "stranger@".to_string(),
        identity(
            "stranger.VRSCTEST@",
            "i92nDT1FzULuXGGXbCt8VHC4qpYc2R1Bfr",
            "RK9izAySZHQAaCEkRmVV4Xtu73uV5sqsZy",
            0,
            0,
        ),
    );
}

/// One string out of a scripted identity object, or empty.
fn text_field(identity: &serde_json::Value, key: &str) -> String {
    identity[key].as_str().unwrap_or_default().to_string()
}

/// One number out of a scripted identity object, or zero.
fn number_field(identity: &serde_json::Value, key: &str) -> u32 {
    u32::try_from(identity[key].as_u64().unwrap_or(0)).unwrap_or(0)
}

/// A scripted VerusID.
///
/// `primary` is the address that controls it, and it is what makes the identity
/// findable by [`ChainReader::identities_with_address`] — the demo build passes
/// the wallet's own address, so the Identities screen has something of yours on
/// it rather than a list of strangers.
///
/// `timelock` carries the raw field, whose meaning depends on `flags`: an
/// absolute height when unlocked, a relative delay when locked. Both spellings
/// are scripted, because the two look identical in a struct and behave nothing
/// alike on screen.
fn identity(name: &str, address: &str, primary: &str, flags: u32, timelock: u32) -> IdentityRecord {
    IdentityRecord {
        fully_qualified_name: name.to_string(),
        identity_address: address.to_string(),
        status: if flags & FLAG_REVOKED != 0 {
            "revoked"
        } else {
            "active"
        }
        .to_string(),
        outpoint: (fixture_txid(9), 0),
        block_height: 1_000_000,
        // The whole object, in the shape `getidentity` returns it — the same
        // shape `content_multimap` and the timelock reader are written against.
        identity: serde_json::json!({
            "name": name,
            "identityaddress": address,
            "parent": "iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq",
            "systemid": "iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq",
            "flags": flags,
            "timelock": timelock,
            "minimumsignatures": 1,
            "primaryaddresses": [primary],
            "revocationauthority": address,
            "recoveryauthority": address,
            "version": 3,
            "contentmap": {},
            // Two entries whose values are hex-encoded UTF-8, which is what
            // real testnet identities carry — so the viewer's "decode it if it
            // decodes" path is reachable without a node.
            "contentmultimap": {
                "i87QZVSS7SosM5choTJE7Dy4SNRt5vAEhr": [
                    "66697273742076616c75652c206d7573742073757276697665"
                ],
                "iGRp1CGkuro3LtGazX8W1PRjVupPVfe8Pv": ["7365636f6e642076616c7565"],
            },
        }),
    }
}

/// When a block at `height` was mined, assuming one minute a block and a tip
/// mined just now. Seconds since the epoch, as a daemon reports it.
fn clock_at(tip: u32, height: u32) -> i64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| i64::try_from(since.as_secs()).unwrap_or(0));
    now - i64::from(tip.saturating_sub(height)) * 60
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
    ///
    /// Both halves: that it fails, and that it fails as a **refusal**. An
    /// unrecognised error shape is classified by `verus_flows` as an unknown
    /// outcome, which would have the wallet record a pending broadcast and go
    /// looking for a transaction that was never sent anywhere.
    #[test]
    fn broadcasting_always_fails_and_says_so_definitely() {
        let chain = MockChain::new(MockState::default());
        let result = chain.send_raw_transaction("0400008085202f8900000000000000000000");
        assert!(
            matches!(result, Err(RpcError::Node { code: -26, .. })),
            "a scripted broadcast must be a refusal, not an unknown outcome: {result:?}",
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
