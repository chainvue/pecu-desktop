//! Holding one node's coins against another's.
//!
//! # What this is for
//!
//! [`crate::network::Network`] says why a node's own account of which chain it
//! is on cannot be checked from that same node: both halves of the name and the
//! chain id arrive in one `getinfo` reply, and a source cannot corroborate
//! itself. The attack that leaves open is a node that calls itself VRSCTEST and
//! answers `getaddressutxos` with **mainnet** outputs. Every check this wallet
//! makes about identity passes; the wallet then builds, signs and broadcasts a
//! transaction that is consensus-valid on mainnet, because the two chains share
//! address version bytes and there is no branch-id separation between them.
//!
//! The only thing that reaches a node lying selectively about the coins is
//! asking a *second* node about the same address. That is what this module
//! does, once per send, for the one address the payment is funded from.
//!
//! # What it covers, and what it does not
//!
//! The **transparent funding set of a spend**, on the two routes the send
//! screen offers that read one: a transparent payment and a `t→z` shield. A
//! shield is in scope precisely because it has no input notes — `shield::plan`
//! funds it from `getaddressutxos` at the transparent address, byte for byte
//! the set a transparent payment reads, so leaving it out would have left the
//! attack open behind one character in the recipient field.
//!
//! `z→z` and `z→t` are out of scope and for a different reason: their inputs
//! are notes, and the notes, the witnesses and the anchor all come from one
//! lightwalletd with no second one to ask.
//!
//! Every other spend this wallet can sign — a conversion, an identity update, a
//! name registration, a currency launch, a resend — still funds from the
//! primary's word alone. Those sites carry a one-line pointer back here saying
//! so. Making the compiler enumerate them needs a second unforgeable token
//! beside [`crate::SpendPermit`] on `Chain::broadcaster`, which is the change
//! this one is deliberately smaller than.
//!
//! # The rule, and why it is not a set comparison
//!
//! **One-directional outpoint membership.** Only the outpoints the primary
//! offers are looked for in the secondary's answer. An outpoint only the
//! *secondary* has is a coin the primary has not indexed yet, or one it has
//! already seen spent — refusing on that would be refusing because the other
//! node is honest and a block ahead. The issue this closes phrases the rule
//! symmetrically ("refusing an outpoint only one of them has"); implemented as
//! written it would fail on the commonest honest state two nodes can be in.
//!
//! **Outpoints only** — `(txid, vout)`. Not `AddressUtxo` equality: that
//! derives over `is_spendable`, which the SDK calls *"the node's own opinion of
//! spendability. Recorded, not relied on"*, and over vector order, and two
//! honest nodes are obliged to agree on neither. Value and script are not
//! compared because they need no comparison — the sighash commits to both, so a
//! node misreporting either produces a transaction the network rejects. That is
//! the half of the SDK's argument for declining to corroborate a UTXO set which
//! does hold. The half that does not is the other one: it reasons about a node
//! *omitting* or *misvaluing* a coin, and this attack *adds* one, whose value
//! and script are perfectly true — about the wrong chain.
//!
//! **Not the tip as a comparison, but the tip as evidence.** Two healthy nodes
//! disagree about the tip most of the time and neither tip is what a signature
//! commits to, so the tips are never compared. The secondary's tip is read for
//! something else entirely: to tell a node that is *behind* from a node that is
//! on *another chain*. A withheld coin in a block above the secondary's tip is
//! a coin the secondary demonstrably has not indexed yet; a withheld coin at or
//! below it is a coin the secondary has looked for the block of and does not
//! have. Only the second is a disagreement, and the difference is what keeps an
//! honest lagging node from being accused of the attack — which on a
//! single-coin address, the commonest wallet shape there is, would otherwise
//! happen every time a payment confirmed into a block the second node had not
//! reached.
//!
//! The one honest case that still lands in the divergent bucket is a secondary
//! that is *ahead* and has already seen the coin spent. It is rare — that coin
//! was spent by this wallet's own key, from a node that would then know about
//! it — and it fails closed rather than open, which is the direction to be
//! wrong in on the money path.
//!
//! **Not the mempool.** Legitimately different between two healthy nodes, and
//! `getaddressutxos` does not report it in the first place.
//!
//! # This is not a type-level guard, and cannot be
//!
//! [`crate::SpendPermit`] is unforgeable because `NodeManager::spend_permit` is
//! its only constructor and that constructor does no I/O — it reads what a
//! probe already recorded. Corroboration is a network call. So it cannot happen
//! inside the permit, and nothing here widens what a permit means: a permit
//! still says only that the node is ready, identified, on the requested chain
//! and armed. Corroboration is a **runtime check on the prepare/confirm path**,
//! and a reader who assumes the permit covers it will be wrong.

use std::collections::HashSet;

use verus_sdk::money::{Amount, Txid};
use verus_sdk::network::{
    AddressBalance, AddressDelta, AddressUtxo, ChainInfo, ChainReader, ConversionEstimate,
    CurrencyConverter, CurrencyPolicy, CurrencyStateAt, CurrencySummary, IdentityAtAddress,
    IdentityContent, IdentityRecord, MempoolDelta, OfferListing, RpcError,
};

/// One unspent output, identified the only two ways that cannot be restated.
pub type Outpoint = (Txid, u32);

/// What a second source said about the coins the primary offered.
#[derive(Debug)]
pub enum Corroboration {
    /// Every outpoint the primary offered is one the secondary also has.
    Agreed { checked: usize },
    /// The secondary is missing coins, and every one of them is in a block it
    /// has not reached.
    ///
    /// `getaddressutxos` is confirmed-only, so an output in a block one node
    /// has not indexed yet is missing from its answer — and that node's own tip
    /// says so. The safe response is to spend the subset both nodes have and
    /// say how much was left out: refusing outright would punish the commonest
    /// honest state, and filtering silently would let a send-all quietly move
    /// less than the balance on screen.
    ///
    /// `kept` can be zero. A wallet whose only coin confirmed a minute ago
    /// against a second node a block behind lands exactly there, and it is
    /// still lag rather than divergence — which is the whole reason this
    /// verdict is decided by height and not by whether anything survived.
    Lagging {
        withheld: Vec<Outpoint>,
        kept: usize,
        /// The secondary's tip, which is the evidence for this verdict and the
        /// only figure that makes the sentence about it actionable.
        tip: u32,
    },
    /// The secondary has indexed the blocks these coins claim to be in, and
    /// does not have them.
    ///
    /// This is the shape of the attack: two chains' UTXO sets for one address
    /// are disjoint, so a wallet reading mainnet coins from a node calling
    /// itself testnet lands here. Kept apart from [`Corroboration::Lagging`]
    /// because the two have different remedies, and apart from a filter
    /// because filtering to the empty set would surface as "not enough funds",
    /// which is the least useful sentence available for the one case it would
    /// be describing.
    ///
    /// Reached whatever `kept` is. A hostile endpoint that mixes one real coin
    /// in with fifty invented ones is not a node one block behind, and letting
    /// a non-empty `kept` downgrade it to a caption about lag would render the
    /// one signal that the active endpoint is lying as routine.
    Diverged {
        /// The withheld outpoints at or below the secondary's tip — the ones it
        /// has no excuse for. Withheld coins above its tip are lag and are not
        /// counted here, because the number is going into a sentence accusing
        /// somebody's node of serving another chain.
        unexplained: Vec<Outpoint>,
        kept: usize,
    },
    /// The secondary could not answer.
    ///
    /// **Not a pass.** The SDK's own second source puts it plainly: *"a source
    /// that cannot answer is a failure, not a pass. The point is a corroborated
    /// answer; an uncorroborated one silently substituted would make the whole
    /// thing decorative the first time a node went down."* The caller decides
    /// what to do with that, and the message it shows has to distinguish "this
    /// endpoint cannot answer the question" from "these two nodes disagree", or
    /// somebody will go and chase the wrong problem.
    Unavailable { reason: RpcError },
}

/// Ask `secondary` which of `address`'s outputs it also has.
///
/// `offered` is what the **primary** answered for the same address. Nothing
/// here reads the primary — the caller already has that answer, and asking
/// again would be comparing two different moments.
///
/// One request in the ordinary case. A second one — `getblockcount` — only when
/// the two answers differ, because that is the only time the secondary's tip
/// decides anything: it is what separates a node that has not reached a block
/// from a node that has and disagrees about what is in it.
///
/// An empty `offered` costs no request at all. There is nothing to hold the
/// primary to, the send is about to fail for want of coins whatever this said,
/// and a second operator learns nothing about an address this wallet is not
/// going to spend from.
pub fn against(
    secondary: &impl ChainReader,
    address: &str,
    offered: &[AddressUtxo],
) -> Corroboration {
    if offered.is_empty() {
        return Corroboration::Agreed { checked: 0 };
    }

    let held: HashSet<Outpoint> = match secondary.address_utxos(&[address]) {
        Ok(found) => found
            .into_iter()
            .map(|utxo| (utxo.utxo.txid, utxo.utxo.vout))
            .collect(),
        Err(reason) => return Corroboration::Unavailable { reason },
    };

    let missing: Vec<&AddressUtxo> = offered
        .iter()
        .filter(|utxo| !held.contains(&(utxo.utxo.txid, utxo.utxo.vout)))
        .collect();
    let kept = offered.len() - missing.len();
    if missing.is_empty() {
        return Corroboration::Agreed { checked: kept };
    }

    // Only now, and only because the verdict turns on it.
    let tip = match secondary.block_count() {
        Ok(tip) => tip,
        Err(reason) => return Corroboration::Unavailable { reason },
    };

    let unexplained: Vec<Outpoint> = missing
        .iter()
        .filter(|utxo| utxo.height <= tip)
        .map(|utxo| (utxo.utxo.txid, utxo.utxo.vout))
        .collect();
    let withheld: Vec<Outpoint> = missing
        .iter()
        .map(|utxo| (utxo.utxo.txid, utxo.utxo.vout))
        .collect();

    if unexplained.is_empty() {
        Corroboration::Lagging {
            withheld,
            kept,
            tip,
        }
    } else {
        Corroboration::Diverged { unexplained, kept }
    }
}

/// The outpoints of `offered` that `secondary` also has.
///
/// The set a build may fund from. Separate from [`against`] so the caller
/// reports and filters from one answer rather than asking twice.
pub fn agreed_outpoints(offered: &[AddressUtxo], withheld: &[Outpoint]) -> HashSet<Outpoint> {
    // A set rather than a scan of the slice per coin: the same line, and it
    // stops being quadratic on the wallet shape that reaches it — a swept
    // address with hundreds of small outputs against a second node a block
    // behind.
    let withheld: HashSet<Outpoint> = withheld.iter().copied().collect();
    offered
        .iter()
        .map(|utxo| (utxo.utxo.txid, utxo.utxo.vout))
        .filter(|outpoint| !withheld.contains(outpoint))
        .collect()
}

/// A reader that can only offer coins a second node has confirmed exist.
///
/// # Why a filtering reader rather than a check after the build
///
/// Because a check after the build has a window in it. Coin selection would
/// have already chosen its inputs, the transaction would already be signed, and
/// the check could only refuse the whole thing — including in the ordinary case
/// where one coin of six is simply newer than the second node. Filtering the
/// source instead means the signed transaction is corroborated **by
/// construction**: the builder is handed a reader that is incapable of offering
/// an uncorroborated outpoint, so there is no order of operations in which one
/// gets selected.
///
/// It wraps the primary rather than replacing it. Every other question — the
/// tip, the mempool, coinbase provenance — is still the primary's to answer,
/// which is what keeps this one extra request rather than a doubling of the
/// send path.
///
/// # It answers about one address
///
/// The allowed set was built for the funding address of one send, so it is
/// applied to that address and to nothing else: an output at any other address
/// is passed through exactly as the primary reported it. Filtering those too
/// would look fail-closed and is not — it would silently answer "no coins" to a
/// caller asking a question this type never checked, which is a wrong answer
/// dressed as a safe one. A future caller that needs a second address checked
/// has to say so by building a second one of these.
pub struct Corroborated<'a, R> {
    primary: &'a R,
    /// The address `allowed` was built for. Nothing else is filtered.
    address: String,
    allowed: HashSet<Outpoint>,
}

impl<'a, R: ChainReader> Corroborated<'a, R> {
    pub fn new(primary: &'a R, address: &str, allowed: HashSet<Outpoint>) -> Self {
        Self {
            primary,
            address: address.to_string(),
            allowed,
        }
    }
}

/// Forward every `ChainReader` question to the primary, unchanged.
///
/// A macro for the same reason [`crate::Chain`] uses one: the trait has 29
/// methods, and the one that matters here is the one written out by hand below.
/// Hand-writing the other 28 is how one of them quietly stops matching.
macro_rules! passthrough {
    ($($method:ident ( $($arg:ident : $ty:ty),* ) -> $ret:ty;)*) => {
        $(
            fn $method(&self $(, $arg: $ty)*) -> Result<$ret, RpcError> {
                self.primary.$method($($arg),*)
            }
        )*
    };
}

impl<R: ChainReader> ChainReader for Corroborated<'_, R> {
    /// The one filtered answer.
    ///
    /// A coin the primary offers *at the corroborated address* that the second
    /// source has never heard of is dropped here, before anything selects it.
    /// An output at any other address is the primary's answer unchanged — see
    /// the note on the type about why that is the honest direction rather than
    /// the lax one.
    fn address_utxos(&self, addresses: &[&str]) -> Result<Vec<AddressUtxo>, RpcError> {
        Ok(self
            .primary
            .address_utxos(addresses)?
            .into_iter()
            .filter(|utxo| {
                utxo.address != self.address
                    || self.allowed.contains(&(utxo.utxo.txid, utxo.utxo.vout))
            })
            .collect())
    }

    passthrough! {
        chain_info() -> ChainInfo;
        block_count() -> u32;
        best_block_hash() -> String;
        block_hash(height: u32) -> String;
        mempool() -> Vec<String>;
        block(height_or_hash: &str) -> serde_json::Value;
        address_deltas(addresses: &[&str], range: Option<(u32, u32)>) -> Vec<AddressDelta>;
        address_mempool(addresses: &[&str]) -> Vec<MempoolDelta>;
        address_balance(addresses: &[&str]) -> AddressBalance;
        currency(name_or_id: &str) -> CurrencyPolicy;
        currency_definition(name_or_id: &str) -> CurrencySummary;
        estimate_conversion(from: &str, to: &str, amount: &str, via: Option<&str>) -> ConversionEstimate;
        currency_state(name_or_id: &str) -> serde_json::Value;
        currency_state_range(name_or_id: &str, from: u32, to: u32, step: u32) -> Vec<CurrencyStateAt>;
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
}

#[cfg(test)]
mod tests {
    use verus_flows::testing::ScriptedReader;

    use super::*;

    /// The address both scripted nodes answer about. Any well-formed R-address
    /// does — nothing here parses it, and `ScriptedReader` keys its answers on
    /// the string.
    const ADDRESS: &str = "RQVsJRf98iq8YmRQdehzRcbLGHEx6YfjdH";

    /// What the primary offers, as the corroborator receives it.
    fn offered(reader: &ScriptedReader) -> Vec<AddressUtxo> {
        reader
            .address_utxos(&[ADDRESS])
            .expect("a scripted node answers")
    }

    /// A node that answers every other question and refuses one of these two.
    ///
    /// `ScriptedReader` has no builder for a failing read, and the cases matter
    /// more than the others: a filtering public proxy that does not expose
    /// `getaddressutxos` is the realistic way a second source goes permanently
    /// silent, and treating silence as agreement is precisely the failure this
    /// module exists to avoid. The tip is refusable separately because the
    /// verdict now turns on it — a node that will not say how far it has got
    /// cannot tell lag from divergence either.
    struct Refuses {
        primary: ScriptedReader,
        /// Built fresh per call, because `RpcError` is not `Clone`.
        utxos: Option<fn() -> RpcError>,
        tip: Option<fn() -> RpcError>,
    }

    impl Refuses {
        fn utxos(refuse: fn() -> RpcError) -> Self {
            Self {
                primary: ScriptedReader::new(1_000),
                utxos: Some(refuse),
                tip: None,
            }
        }

        fn tip(refuse: fn() -> RpcError) -> Self {
            Self {
                // Holds nothing, so every coin the primary offers is missing
                // and the tip is what the verdict would turn on.
                primary: ScriptedReader::new(1_000),
                utxos: None,
                tip: Some(refuse),
            }
        }
    }

    impl ChainReader for Refuses {
        fn address_utxos(&self, addresses: &[&str]) -> Result<Vec<AddressUtxo>, RpcError> {
            match self.utxos {
                Some(refuse) => Err(refuse()),
                None => self.primary.address_utxos(addresses),
            }
        }

        fn block_count(&self) -> Result<u32, RpcError> {
            match self.tip {
                Some(refuse) => Err(refuse()),
                None => self.primary.block_count(),
            }
        }

        passthrough! {
            chain_info() -> ChainInfo;
            best_block_hash() -> String;
            block_hash(height: u32) -> String;
            mempool() -> Vec<String>;
            block(height_or_hash: &str) -> serde_json::Value;
            address_deltas(addresses: &[&str], range: Option<(u32, u32)>) -> Vec<AddressDelta>;
            address_mempool(addresses: &[&str]) -> Vec<MempoolDelta>;
            address_balance(addresses: &[&str]) -> AddressBalance;
            currency(name_or_id: &str) -> CurrencyPolicy;
            currency_definition(name_or_id: &str) -> CurrencySummary;
            estimate_conversion(from: &str, to: &str, amount: &str, via: Option<&str>) -> ConversionEstimate;
            currency_state(name_or_id: &str) -> serde_json::Value;
            currency_state_range(name_or_id: &str, from: u32, to: u32, step: u32) -> Vec<CurrencyStateAt>;
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
    }

    /// The rule is one-directional, and this is the half the issue got wrong.
    ///
    /// A coin only the *second* node has is the second node being a block
    /// ahead, or the primary having already seen it spent. Refusing on it would
    /// be refusing because the other node is honest, which would make the guard
    /// fire on the commonest state two nodes are ever in.
    #[test]
    fn an_outpoint_only_the_second_source_has_is_not_a_disagreement() {
        let primary = ScriptedReader::new(1_000).with_utxo(ADDRESS, 900, 100_000);
        let secondary = ScriptedReader::new(1_001)
            .with_utxo(ADDRESS, 900, 100_000)
            .with_utxo(ADDRESS, 901, 200_000);

        match against(&secondary, ADDRESS, &offered(&primary)) {
            Corroboration::Agreed { checked } => assert_eq!(checked, 1),
            other => panic!("an honest node a block ahead was treated as a disagreement: {other:?}"),
        }
    }

    /// The same statement from the other side: what the older coins are worth
    /// is unaffected by a block one node has and the other has not.
    #[test]
    fn two_nodes_a_block_apart_still_agree_about_the_older_coins() {
        let older = |reader: ScriptedReader| {
            reader
                .with_utxo(ADDRESS, 800, 100_000)
                .with_utxo(ADDRESS, 801, 200_000)
        };
        let primary = older(ScriptedReader::new(1_000));
        let secondary = older(ScriptedReader::new(1_001)).with_utxo(ADDRESS, 1_001, 50_000);

        match against(&secondary, ADDRESS, &offered(&primary)) {
            Corroboration::Agreed { checked } => assert_eq!(checked, 2),
            other => panic!("the wrong verdict: {other:?}"),
        }
    }

    /// A coin in a block the second node has not reached is lag, and the second
    /// node's own tip is the evidence.
    ///
    /// `getaddressutxos` is confirmed-only, so a payment that confirmed one
    /// block ago is missing from the answer of anything a block behind. Spend
    /// the rest, say what was left out.
    #[test]
    fn a_coin_above_the_second_sources_tip_is_lag_rather_than_a_disagreement() {
        let primary = ScriptedReader::new(1_001)
            .with_utxo(ADDRESS, 900, 100_000)
            .with_utxo(ADDRESS, 1_001, 200_000);
        let secondary = ScriptedReader::new(1_000).with_utxo(ADDRESS, 900, 100_000);

        match against(&secondary, ADDRESS, &offered(&primary)) {
            Corroboration::Lagging {
                withheld,
                kept,
                tip,
            } => {
                assert_eq!(withheld.len(), 1);
                assert_eq!(kept, 1);
                assert_eq!(tip, 1_000);
            }
            other => panic!("a node one block behind was accused of something: {other:?}"),
        }
    }

    /// And it is still lag when nothing survives it, which is the commonest
    /// wallet shape there is.
    ///
    /// One coin at the address — a fresh key, a swept key, a merchant
    /// forwarding a payment that just confirmed — and a second node that has
    /// not reached its block. Deciding the verdict on "did anything survive"
    /// would make that pair unreachable by any answer except the one that
    /// accuses them, on a guard with no off switch.
    #[test]
    fn a_lagging_second_source_is_still_lagging_when_it_leaves_nothing_to_spend() {
        let primary = ScriptedReader::new(1_001).with_utxo(ADDRESS, 1_001, 100_000);
        let secondary = ScriptedReader::new(1_000);

        match against(&secondary, ADDRESS, &offered(&primary)) {
            Corroboration::Lagging { kept, tip, .. } => {
                assert_eq!(kept, 0);
                assert_eq!(tip, 1_000);
            }
            other => panic!("a single-coin address against a lagging node was refused: {other:?}"),
        }
    }

    /// Total disagreement at heights the second node has indexed is its own
    /// answer, not a filter down to nothing.
    ///
    /// Two chains' unspent outputs for one address are disjoint in both
    /// directions, so this is the shape the wrong-chain endpoint produces.
    /// Filtering to the empty set instead would reach the send form as "not
    /// enough funds", which is the least useful sentence available for the one
    /// case it would be describing.
    #[test]
    fn a_second_source_that_recognises_nothing_the_primary_offered_is_a_refusal_and_not_a_filter() {
        let primary = ScriptedReader::new(1_000)
            .with_utxo(ADDRESS, 900, 100_000)
            .with_utxo(ADDRESS, 901, 200_000);
        let secondary = ScriptedReader::new(1_000)
            .with_utxo(ADDRESS, 700, 100_000)
            .with_utxo(ADDRESS, 701, 200_000);

        match against(&secondary, ADDRESS, &offered(&primary)) {
            Corroboration::Diverged { unexplained, kept } => {
                assert_eq!(unexplained.len(), 2);
                assert_eq!(kept, 0);
            }
            other => panic!("a node serving another chain's coins was not refused: {other:?}"),
        }
    }

    /// One real coin does not buy a hostile endpoint a caption about lag.
    ///
    /// A node that offers the wallet's own coin alongside a pile of invented
    /// ones is not a node a block behind, and a verdict decided by "did
    /// anything survive" would say it was — rendering the only signal that the
    /// active endpoint is lying as the same tertiary sentence an honest node
    /// gets.
    #[test]
    fn a_node_that_mixes_one_real_coin_in_with_invented_ones_is_still_a_disagreement() {
        let primary = ScriptedReader::new(1_000)
            .with_utxo(ADDRESS, 900, 100_000)
            .with_utxo(ADDRESS, 700, 200_000)
            .with_utxo(ADDRESS, 701, 300_000);
        // Holds the first coin and nothing else, and has long since indexed the
        // blocks the other two claim to be in.
        let secondary = ScriptedReader::new(1_000).with_utxo(ADDRESS, 900, 100_000);

        match against(&secondary, ADDRESS, &offered(&primary)) {
            Corroboration::Diverged { unexplained, kept } => {
                assert_eq!(unexplained.len(), 2);
                assert_eq!(kept, 1);
            }
            other => panic!("a mixed set was downgraded to lag: {other:?}"),
        }
    }

    /// A node that cannot answer has not agreed.
    #[test]
    fn a_second_source_that_cannot_answer_is_reported_as_unchecked_and_never_as_agreement() {
        let primary = ScriptedReader::new(1_000).with_utxo(ADDRESS, 900, 100_000);
        let secondary = Refuses::utxos(|| RpcError::Transport("connection reset".to_string()));

        match against(&secondary, ADDRESS, &offered(&primary)) {
            Corroboration::Unavailable { .. } => {}
            other => panic!("silence was read as an answer: {other:?}"),
        }
    }

    /// And the specific silence that is easiest to mistake for an answer.
    ///
    /// `-32601` from a filtering proxy comes back as an empty list to anything
    /// that swallows it — and an empty list from the second node, compared
    /// naively, would withhold every coin the primary offered and read as the
    /// attack. `Node::record_failure` is deliberately lenient about this error;
    /// the corroborator must not inherit that leniency, and the two states have
    /// to be told apart or somebody will chase the wrong problem.
    #[test]
    fn a_second_source_that_refuses_getaddressutxos_is_unavailable_rather_than_empty() {
        let primary = ScriptedReader::new(1_000).with_utxo(ADDRESS, 900, 100_000);
        let secondary = Refuses::utxos(|| RpcError::MethodUnavailable {
            method: "getaddressutxos",
        });

        match against(&secondary, ADDRESS, &offered(&primary)) {
            Corroboration::Unavailable { reason } => assert!(
                matches!(reason, RpcError::MethodUnavailable { .. }),
                "the reason was lost: {reason}",
            ),
            other => panic!("a refused method was read as an empty UTXO set: {other:?}"),
        }
    }

    /// A node that will not say how far it has got cannot be told apart from a
    /// node on another chain, so it is neither.
    ///
    /// The tip is the whole evidence for the lag verdict. Guessing it — in
    /// either direction — would either accuse an honest node or excuse a lying
    /// one, and both are worse than saying the check did not happen.
    #[test]
    fn a_second_source_that_will_not_say_how_far_it_has_got_is_unavailable_rather_than_lagging() {
        let primary = ScriptedReader::new(1_000).with_utxo(ADDRESS, 900, 100_000);
        let secondary = Refuses::tip(|| RpcError::Transport("timed out".to_string()));

        match against(&secondary, ADDRESS, &offered(&primary)) {
            Corroboration::Unavailable { .. } => {}
            other => panic!("a missing tip was guessed at: {other:?}"),
        }
    }

    /// `AddressUtxo` derives `PartialEq` over `is_spendable`, which the SDK
    /// calls the node's own opinion and says is recorded rather than relied on.
    /// Two honest nodes are free to disagree about it — and the wallet does not
    /// take either node's word for it anyway, since `spendable_at` decides
    /// maturity from the tip. A comparison that included it would report a
    /// disagreement about a field neither side is being trusted on.
    #[test]
    fn the_comparison_ignores_the_nodes_own_opinion_of_spendability() {
        let primary = ScriptedReader::new(1_000).with_utxo(ADDRESS, 900, 100_000);
        let secondary = ScriptedReader::new(1_000).with_utxo(ADDRESS, 900, 100_000);

        // The same coin, with the two nodes' opinions of it set opposite. The
        // double has no builder for the flag, so it is flipped on the answer.
        let mut mine = offered(&primary);
        for utxo in &mut mine {
            utxo.is_spendable = false;
        }
        assert_ne!(
            mine,
            offered(&secondary),
            "the two answers were made identical, so this test proves nothing",
        );

        match against(&secondary, ADDRESS, &mine) {
            Corroboration::Agreed { checked } => assert_eq!(checked, 1),
            other => panic!("one node's opinion of spendability decided a spend: {other:?}"),
        }
    }

    /// And the other thing `AddressUtxo` equality would drag in: vector order,
    /// which no node promises and two nodes need not share.
    ///
    /// Only the primary's order can be varied here — `ScriptedReader` derives a
    /// coin's txid from its insertion index, so two readers cannot describe the
    /// same coins in different orders. The secondary's side is order-free by
    /// construction: [`against`] collects its answer into a set before anything
    /// is compared, which is the property this pins from the other direction.
    #[test]
    fn the_comparison_ignores_the_order_the_outputs_arrive_in() {
        let primary = ScriptedReader::new(1_000)
            .with_utxo(ADDRESS, 900, 100_000)
            .with_utxo(ADDRESS, 901, 200_000);
        let secondary = ScriptedReader::new(1_000)
            .with_utxo(ADDRESS, 900, 100_000)
            .with_utxo(ADDRESS, 901, 200_000);

        let mut mine = offered(&primary);
        mine.reverse();
        assert_ne!(mine, offered(&secondary));

        match against(&secondary, ADDRESS, &mine) {
            Corroboration::Agreed { checked } => assert_eq!(checked, 2),
            other => panic!("the order the outputs arrived in decided a spend: {other:?}"),
        }
    }

    /// The filter, which is the half that reaches a signed transaction.
    ///
    /// A verdict nobody applies is a comment. This asserts the reader handed to
    /// the builder cannot offer the coin the second node never heard of, which
    /// is what makes the transaction corroborated by construction rather than
    /// checked afterwards.
    #[test]
    fn the_filtered_reader_cannot_offer_a_coin_the_second_source_withheld() {
        let primary = ScriptedReader::new(1_001)
            .with_utxo(ADDRESS, 900, 100_000)
            .with_utxo(ADDRESS, 1_001, 200_000);
        let secondary = ScriptedReader::new(1_000).with_utxo(ADDRESS, 900, 100_000);

        let mine = offered(&primary);
        let Corroboration::Lagging { withheld, .. } = against(&secondary, ADDRESS, &mine) else {
            panic!("the scripted pair does not disagree the way this test needs");
        };

        let reader = Corroborated::new(&primary, ADDRESS, agreed_outpoints(&mine, &withheld));
        let filtered = reader
            .address_utxos(&[ADDRESS])
            .expect("the primary answers");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].utxo.satoshis.to_sat(), 100_000);
        // And everything else is still the primary's to answer.
        assert_eq!(
            reader.block_count().expect("the primary answers"),
            primary.block_count().expect("the primary answers"),
        );
    }

    /// An address this was not built for gets the primary's answer, unchanged.
    ///
    /// The fail-closed-looking alternative — filter everything against one
    /// address's allowed set — answers "no coins" to a question nobody checked,
    /// which is a wrong answer wearing a safe one's clothes. Today no caller
    /// asks; the type should not depend on that staying true.
    #[test]
    fn the_filtered_reader_passes_through_an_address_it_was_not_built_for() {
        const OTHER: &str = "RJmMPWfLZDoxrCRDbTLMSy6QCn9jsx7Mzy";
        let primary = ScriptedReader::new(1_000)
            .with_utxo(ADDRESS, 900, 100_000)
            .with_utxo(OTHER, 900, 300_000);

        // An allowed set that vouches for nothing at all.
        let reader = Corroborated::new(&primary, ADDRESS, HashSet::new());

        assert!(
            reader
                .address_utxos(&[ADDRESS])
                .expect("the primary answers")
                .is_empty(),
            "the corroborated address was not filtered",
        );
        let other = reader
            .address_utxos(&[OTHER])
            .expect("the primary answers");
        assert_eq!(
            other.len(),
            1,
            "an address this reader never checked was silently answered as empty",
        );
    }
}
