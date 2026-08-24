//! Building, reviewing and broadcasting a payment.
//!
//! # Three properties this module exists to preserve
//!
//! **The dry run is enforced by the type system.** `prepare_send` takes a
//! `ChainReader` and no `Broadcaster`, so the value it returns is *incapable*
//! of reaching a node. Nothing here has to remember not to send.
//!
//! **The review is decoded from the signed bytes, not echoed from the form.**
//! Between what someone typed and what got signed sit coin selection, the fee
//! rule and change placement. A review that replays the form cannot show a
//! mistake in any of them, which makes it a confirmation dialog rather than a
//! review. So [`review`] deserializes the transaction that was actually built
//! and reads its outputs back out.
//!
//! **An uncertain broadcast is never rebuilt.** A transport failure on
//! `sendrawtransaction` is genuinely ambiguous: the node may have accepted and
//! relayed it before the connection dropped. Rebuilding would select different
//! coins, produce different bytes, and could spend twice. The only safe move is
//! to re-send the *same bytes*, which is why [`Prepared`] keeps them.

use pecu_chain::{corroborate, Chain, Corroboration, SpendPermit, SpendRefused};
use pecu_keystore::{Vault, VaultError};
use pecu_protocol::{NoteVm, ReviewOutputVm, SendDraft, SendReviewVm};
use verus_sdk::money::{Amount, DEFAULT_FEE_PER_KB};
use verus_sdk::network::{self, ChainReader, FlowError, Funding, Sent, Unsent};
use verus_sdk::verus_keys::{Address, AddressKind};
// `estimate_fee` is not in any of the curated `verus_sdk` facades — `money` and
// `send` expose the types and the builders, not the fee heuristic. Reaching
// through `verus_tx`, which the SDK re-exports wholesale for exactly this, is
// deliberate: see `resolve_send_all` for why this module has to price a
// transaction the builder has not been asked to build yet.
use verus_sdk::verus_tx::estimate_fee;

use crate::portfolio::coins;

#[derive(Debug, thiserror::Error)]
pub enum SendError {
    #[error("that is not an address this wallet can pay")]
    BadAddress,

    /// A shielded address, which this build cannot pay.
    ///
    /// Its own variant because [`Self::BadAddress`] would be a lie now. A `zs…`
    /// **is** a Verus address — this wallet derives one and shows it on its own
    /// Receive screen — and telling somebody it is not, two screens later, is
    /// worse than saying nothing. What is true is narrower: paying one needs a
    /// Groth16 output proof, and the SDK's `prover` feature that builds it is
    /// not compiled into this build. That applies to every direction involving
    /// a shielded address, `t→z` included, so having a transparent balance does
    /// not help.
    /// The Sapling proving parameters are not on this machine.
    ///
    /// Checked **before** the work is dispatched rather than inside it. The
    /// parameters are ~50 MB and loading them takes seconds; discovering they
    /// are absent after the button has already gone quiet would look like a
    /// hang, and the honest answer — "this needs a file you do not have" —
    /// would arrive last instead of first.
    #[error("the Sapling proving parameters are not on this machine")]
    ParamsMissing,

    /// Proving or building a shielded transaction failed.
    #[error("{0}")]
    Shielded(String),
    #[error("that is not an amount")]
    BadAmount,
    #[error("send nothing and nothing happens")]
    NothingToSend,

    /// Everything this key holds is worth less than it costs to move.
    ///
    /// Only reachable from a send-all: an ordinary payment of an amount the
    /// fee tips out of reach is the SDK's `InsufficientFunds`, which can quote
    /// both figures. This one cannot quote the fee, because there is no
    /// transaction to price — see [`resolve_send_all`].
    #[error("there is not enough here to cover the network fee")]
    NotEnoughForFee,
    #[error(transparent)]
    Vault(#[from] VaultError),
    #[error(transparent)]
    Flow(#[from] FlowError),

    /// The spending guard's own vocabulary, raised from the build rather than
    /// from the permit.
    ///
    /// Only the corroboration refusals reach here. They cannot come from
    /// `NodeManager::spend_permit`, because answering them costs a network call
    /// and that constructor deliberately does none — see
    /// [`pecu_chain::corroborate`]. Carried as [`pecu_chain::SpendRefused`]
    /// rather than as more `SendError` variants so that the send form and the
    /// broadcast gate say the same sentence about the same fact.
    #[error(transparent)]
    Refused(#[from] SpendRefused),
}

/// A signed payment that has not been sent, and what it is for.
///
/// **Never leaves the core.** The UI gets a [`SendReviewVm`] and a ticket
/// number; the bytes stay here. A UI that held the signed hex would be a UI
/// that could be made to send it.
pub struct Prepared {
    pub signed: Signed,
    pub to: String,
    pub amount: Amount,
    /// Which of the four things this payment is.
    pub route: pecu_protocol::Route,
    /// Nullifiers this payment will publish, for a shielded spend.
    ///
    /// Empty for every other route. Carried so the core can mark the notes
    /// spent the moment the network accepts the transaction — see
    /// `Shielded::note_spent`. Without it a second spend before the first
    /// confirms picks the same note and the daemon refuses the whole
    /// transaction with `bad-txns-sapling-nullifier-exists`, after the prover
    /// has been paid for.
    pub spends: Vec<[u8; 32]>,
    /// The VerusID name `to` was resolved from, when it was typed as a name.
    /// Empty otherwise. Carried so the review can show the question as well as
    /// the answer.
    pub name: String,
    /// The endpoint that held the funding node's coins to account, or empty
    /// when nothing did.
    ///
    /// Read twice. The review names it, because a guard nobody can see is a
    /// guard nobody notices has stopped working; and `confirm_send` refuses to
    /// broadcast bytes that were built without one when the node list says one
    /// was required. Empty on the two routes that spend shielded notes, whose
    /// inputs come from one lightwalletd with no second one to ask — but *not*
    /// on a `t→z` shield, which is funded from transparent coins and is checked
    /// like any other. See [`pecu_chain::corroborate`].
    pub corroborated_by: String,
    /// How many of the funding node's outputs the second source had not
    /// reached the block of, and which this transaction therefore does not
    /// spend.
    ///
    /// Almost always zero, and it can only ever be lag: a second source that
    /// disagrees about a block it *has* indexed does not produce a filtered
    /// payment, it refuses the whole one. Carried out so the review can say so
    /// — a send-all that quietly moves less than the balance on screen is its
    /// own bug.
    pub withheld: usize,
}

/// Signed bytes, and the only way to send them.
///
/// # Why the four routes share one type
///
/// Everything downstream of signing is identical for all of them: the bytes are
/// written to the pending ledger *before* the broadcast, the broadcast needs a
/// `SpendPermit`, and the outcome is a txid and a fee. Modelling the routes as
/// four parallel paths would mean four chances to forget the ledger, and the
/// ledger is what stops a crash mid-send from losing bytes that may already be
/// propagating.
///
/// So the difference is confined to this enum, and it is exactly two things:
/// what the bytes are, and which call sends them.
pub enum Signed {
    /// `R → R`. The transparent path, unchanged.
    Transparent(Unsent<Sent>),
    /// `R → z`. Proven by `verus-sapling`, transparent inputs signed after.
    Shield(crate::shield::Prepared),
    /// `z → z` or `z → R`. Complete when it is built — a shielded spend has no
    /// transparent inputs, so there is nothing left to sign.
    Shielded(Unsent<verus_sdk::light::ShieldedSpent>),
}

impl Signed {
    /// The bytes, for the pending ledger.
    pub fn hex(&self) -> &str {
        match self {
            Self::Transparent(unsent) => &unsent.hex,
            Self::Shield(shield) => &shield.hex,
            Self::Shielded(unsent) => &unsent.hex,
        }
    }

    /// The transaction id, computed locally rather than taken from a reply.
    pub fn txid(&self) -> &str {
        match self {
            Self::Transparent(unsent) => &unsent.txid,
            Self::Shield(shield) => &shield.txid,
            Self::Shielded(unsent) => &unsent.txid,
        }
    }

    /// What the miner takes.
    pub fn fee(&self) -> Amount {
        match self {
            Self::Transparent(unsent) => unsent.outcome.fee,
            Self::Shield(shield) => shield.fee,
            Self::Shielded(unsent) => Amount::from_sat(unsent.outcome.fee),
        }
    }

    /// What comes back — as coin for a transparent send or a shield, and as a
    /// new note for a shielded spend.
    pub fn change(&self) -> Amount {
        match self {
            Self::Transparent(unsent) => unsent.outcome.change,
            Self::Shield(shield) => shield.change,
            Self::Shielded(unsent) => Amount::from_sat(unsent.outcome.change),
        }
    }

    /// Send it, once.
    ///
    /// Every arm ends in the same place: a `Broadcaster`, which in this wallet
    /// can only be obtained from a [`SpendPermit`].
    fn broadcast(self, chain: &Chain, permit: &SpendPermit) -> Result<Sent, FlowError> {
        let broadcaster = chain.broadcaster(permit);
        match self {
            Self::Transparent(unsent) => unsent.broadcast(&broadcaster),
            Self::Shield(shield) => {
                verus_sdk::network::broadcast(&broadcaster, &shield.hex, &shield.txid)?;
                Ok(Sent {
                    txid: shield.txid,
                    fee: shield.fee,
                    change: shield.change,
                    hex: shield.hex,
                })
            }
            Self::Shielded(unsent) => {
                let spent = unsent.broadcast(&broadcaster)?;
                Ok(Sent {
                    txid: spent.txid,
                    fee: Amount::from_sat(spent.fee),
                    change: Amount::from_sat(spent.change),
                    hex: spent.hex,
                })
            }
        }
    }
}

// ── Validating what has been typed ──────────────────────────────────────────

/// Check a draft without touching the network.
///
/// Address parsing and amount parsing are both offline and both exact, so this
/// can run on every keystroke. What it deliberately does **not** do is check
/// the balance: that needs the chain, and a form that goes red because a
/// refresh is in flight is a form that punishes typing.
pub fn validate(draft: &SendDraft, spendable: Amount) -> pecu_protocol::DraftValidationVm {
    // Codes, not sentences. See `NoteVm`: this decides *what is true about the
    // address*, which is the core's job, and leaves the wording to the side
    // that knows how much room the line has and what language it is in.
    // Checked first and by decoding rather than by prefix, so a `zs1…` that
    // fails its checksum falls through to the ordinary typo path instead of
    // being explained as a shielded payment.
    let to_shielded = verus_sdk::light::zaddr::decode(draft.to.trim()).is_ok();

    let (to_valid, to_note) = match draft.to.trim() {
        "" => (false, NoteVm::none()),
        _ if to_shielded => (true, NoteVm::plain("address-shielded")),
        text => match text.parse::<Address>() {
            Ok(address) => match address.kind() {
                AddressKind::PubKeyHash => (true, NoteVm::plain("address-transparent")),
                // An identity is a legitimate destination — `prepare_send`
                // parses one and pays it. Saying so beats a bare tick.
                AddressKind::Identity => (true, NoteVm::plain("address-verusid")),
                AddressKind::ScriptHash => (true, NoteVm::plain("address-script")),
            },
            Err(_) => (false, NoteVm::plain("address-unparsable")),
        },
    };

    // The route, not the pool, is what decides whether a send-all can be
    // served — and it is what `Core::refuses_send_all` branches on. Branching
    // on the pool here said "ready" for a transparent balance paying a `zs1…`
    // and then watched the core refuse it, which is precisely the disagreement
    // the offline refusal below exists to prevent.
    let route = pecu_protocol::Route::of(draft.from_pool, to_shielded);

    // Sending everything has no typed amount to judge, and the form has no
    // field to type one into. What is still knowable offline is the one refusal
    // that matters: a balance at or under the cheapest possible fee cannot pay
    // for any transaction at all. Deliberately without a figure — the fee is
    // not known until coins have been selected, and quoting a guess is the
    // mistake the amount field was taken off the form to avoid.
    let (amount_valid, amount_note) = if draft.send_all {
        // Refused here as well as at the builder, so the button is disabled
        // rather than pressed into a refusal. Two codes rather than one,
        // because the two refusals are about different halves of the payment
        // and a sentence that fits one is false about the other: a shielded
        // *source* is money this cannot sweep, a shielded *destination* is
        // money it cannot deliver.
        match route {
            pecu_protocol::Route::Transparent => {
                if spendable.to_sat() > cheapest_transparent_fee() {
                    (true, NoteVm::none())
                } else {
                    (false, NoteVm::plain("amount-below-fee"))
                }
            }
            // `R → z`. The coins being swept really are the transparent ones,
            // so the sentence about paying from the shielded balance would be
            // telling somebody to fix the half that is not wrong. What is
            // wrong is the destination: shielding runs through a different
            // builder with a different fee, and `resolve_send_all` prices a
            // transparent payment.
            pecu_protocol::Route::Shield => (false, NoteVm::plain("send-all-shielded-recipient")),
            // `z → z` and `z → R`. See `Core::refuses_send_all`: the shielded
            // fee does not depend on the input count, but a balance spread over
            // more than ten notes cannot all move at once, and what "everything"
            // means then is not a question this commit answers.
            pecu_protocol::Route::Private | pecu_protocol::Route::Unshield => {
                (false, NoteVm::plain("send-all-transparent-only"))
            }
        }
    } else {
        match draft.amount.trim() {
            "" => (false, NoteVm::none()),
            text => match Amount::from_coins_str(text) {
                Ok(amount) if amount.is_zero() => (false, NoteVm::plain("amount-zero")),
                // The figure travels with the code, already spelled: money is
                // formatted in exactly one place in this workspace and the
                // interface is not a second one.
                Ok(amount) if amount > spendable => (
                    false,
                    NoteVm::with("amount-above-spendable", [coins(spendable)]),
                ),
                Ok(_) => (true, NoteVm::none()),
                // The SDK refuses more than eight decimal places rather than
                // rounding, and so does this: a satoshi silently dropped is a
                // satoshi the user did not decide to drop.
                Err(_) => (false, NoteVm::plain("amount-unparsable")),
            },
        }
    };

    pecu_protocol::DraftValidationVm {
        to_valid,
        to_note,
        route,
        amount_valid,
        amount_note,
        // Filled in by the caller, which is the side that knows what this
        // wallet has called the address.
        to_label: String::new(),
        ready: to_valid && amount_valid,
    }
}

// ── Emptying a key ──────────────────────────────────────────────────────────

/// The cheapest a transparent payment can be, whatever it pays for.
///
/// One input, one recipient plus change, plain outputs — the smallest
/// transaction the builder can produce. Derived by asking the SDK rather than
/// written down here, so a rev bump that moves the floor moves this with it.
///
/// A recipient that is a VerusID is priced higher, never lower, so this stays a
/// valid lower bound for every transparent send. That is what makes it safe to
/// use offline: a balance at or under it cannot pay for *any* transaction.
fn cheapest_transparent_fee() -> u64 {
    // The only way this fails is `num_inputs * INPUT_SIZE` overflowing a `u64`,
    // which two constants cannot do. If a future SDK ever made it possible,
    // refusing is the honest direction on a money path — better a send-all that
    // declines than one that promises a fee it could not price.
    estimate_fee(1, 2, DEFAULT_FEE_PER_KB, false).unwrap_or(u64::MAX)
}

/// The amount that empties a key — worked out from the coins, not from the
/// balance.
///
/// # Why this is not `total − fee`
///
/// It reads like it should be, and it is wrong. The transparent fee is a
/// function of the transaction's **size**, so of how many inputs it has;
/// `select_utxos` chooses how many inputs by looping until the selected value
/// covers **the amount** plus the fee so far. So the fee depends on the input
/// count, the input count depends on the amount, and the amount is the thing
/// being derived. The SDK says so itself, in `prepare_send`: *"The fee is not
/// known until selection."*
///
/// Subtracting a fee priced for every coin the key holds always **builds**, and
/// frequently does not **empty**. Past the fee floor each extra input costs
/// about 1 800 satoshis, so selection declines any trailing coin worth less
/// than that: it stops early, at a lower input count and a lower fee, and the
/// difference comes back as a change output. The key still has coins in it, and
/// the review shows change on a payment that was supposed to leave none.
///
/// # The rule that is exact
///
/// Pick the input set first and let the amount fall out of it. Sorted
/// descending by value — the order `select_utxos` puts them in — for each
/// prefix length `k`:
///
/// ```text
/// net(k) = Σ(the k largest) − estimate_fee(k, outputs + 1, …)
/// ```
///
/// and the answer is the largest `net(k)` over every prefix. Nothing else — the
/// amount is the whole return value, and **which** prefix achieves it is never
/// computed here.
///
/// It is exact because at `amount = max net` the selection loop's exit test at
/// step `j` — "is what I have selected at least the amount plus this fee" — is
/// precisely `net(j) ≥ max net`, and a prefix satisfies that only by *being* a
/// maximiser. So selection stops on the first prefix that achieves the maximum,
/// whichever that is, and the change is `Σ − amount − fee = net(j) − amount = 0`
/// to the satoshi.
///
/// That is also why ties in `net` need no handling in the loop below. Several
/// prefixes reaching the same maximum is common — a coin worth exactly what an
/// input costs adds nothing — and the selector, not this function, decides
/// which of them it stops on. Either choice yields the same amount and the same
/// zero change.
///
/// Coins beyond the prefix the selector stops on are excluded because spending
/// them costs more than they are worth. That is the right economic answer and a
/// user-visible one: a key with a few hundred satoshis of dust still reads as
/// non-empty afterwards. The send form says so in as many words — *"A coin
/// worth less than it costs to move stays where it is"* — because without that
/// sentence "everything" is a promise this function does not keep. What is not
/// said is **how much**, and `docs/LATER.md` §13 carries why not.
///
/// The red test for all of this is
/// `sending_everything_leaves_behind_a_coin_that_costs_more_to_spend_than_it_is_worth`
/// in `crates/pecu-core/tests/send_build.rs`: five whole coins and three worth
/// 500 satoshis each, where `total − fee` builds a transaction that hands
/// change back to a key somebody has just been told is empty. It is an
/// integration test rather than a unit test because the property it asserts —
/// no change output — belongs to the SDK's selector, not to the arithmetic
/// here.
///
/// Pure and offline. It reads a `Funding` — the same set the builder will be
/// handed — and returns an amount; it makes no calls and holds no key.
pub fn resolve_send_all(funding: &Funding, has_smart_outputs: bool) -> Result<Amount, SendError> {
    // Descending by value, stably, so "the k largest" here names the same coins
    // as "the first k `select_utxos` takes". Its sort is stable too, and ties
    // keep the caller's order on both sides.
    let mut values: Vec<u64> = funding
        .utxos
        .iter()
        .map(|utxo| utxo.satoshis.to_sat())
        .collect();
    values.sort_by_key(|value| core::cmp::Reverse(*value));

    // One recipient, plus the change output the selector always prices for.
    // Both must match what `plan_transparent_send` will compute or the input
    // count comes out one off, silently.
    let change_outputs = 2;

    // `i128` because the fee is subtracted from a running sum and the early
    // prefixes are legitimately negative — and because a sum of `u64` satoshis
    // has no business wrapping on the money path.
    let mut running: i128 = 0;
    let mut inputs: u64 = 0;
    let mut best: Option<i128> = None;
    for value in &values {
        running += i128::from(*value);
        inputs += 1;
        let fee = estimate_fee(
            inputs,
            change_outputs,
            DEFAULT_FEE_PER_KB,
            has_smart_outputs,
        )
        .map_err(|error| SendError::Flow(error.into()))?;
        let net = running - i128::from(fee);
        // A running maximum and nothing more. No index is kept, because none is
        // needed: see the doc above — the selector lands on the first prefix
        // achieving this figure by its own exit test, so a tie here has no
        // consequence to break.
        best = Some(best.map_or(net, |previous| previous.max(net)));
    }

    let Some(net) = best else {
        // No spendable coins at all. The same refusal as a balance that cannot
        // cover a fee, because from the form it is the same situation: there is
        // nothing here that can be moved.
        return Err(SendError::NotEnoughForFee);
    };
    if net <= 0 {
        return Err(SendError::NotEnoughForFee);
    }

    // Unreachable with a real chain — a total above `u64::MAX` satoshis is more
    // coin than exists — but this is the money path, so it is checked rather
    // than cast.
    u64::try_from(net)
        .map(Amount::from_sat)
        .map_err(|_| SendError::BadAmount)
}

// ── Building ────────────────────────────────────────────────────────────────

/// The second endpoint a send is held against, and the URL to name it by.
///
/// A `Chain` does not carry the URL it was built from, and every sentence this
/// check can produce needs one: the review names the endpoint that did the
/// checking, and each refusal names the endpoint that could not. Carrying it
/// beside the reader is cheaper than teaching `Chain` to remember where it came
/// from, and it keeps the scripted chain — a `Chain` with no URL at all —
/// expressible.
pub struct Corroborator<'a> {
    /// The second node, as something that can be read.
    pub chain: &'a Chain,
    /// Where it lives. Shown on the review, and named in every refusal that is
    /// about *this* endpoint rather than about the funding one.
    pub url: &'a str,
}

/// What a second source vouched for, and what it set aside.
struct Vouched {
    /// The outpoints a build may fund from.
    allowed: std::collections::HashSet<pecu_chain::Outpoint>,
    /// Present when the second source is behind and some coins were left out
    /// because of it. Absent when it vouched for everything.
    behind: Option<Behind>,
}

/// A second source that has not reached the blocks some of these coins are in.
#[derive(Clone, Copy)]
struct Behind {
    count: usize,
    /// The secondary's tip. Carried because "it has only reached block N" is
    /// the difference between a sentence somebody can act on and an accusation.
    tip: u32,
}

/// Build and sign, without sending. **Blocking.**
///
/// The private key exists for the duration of `with_key` and no longer — the
/// build, the signature and the drop all happen inside that closure. There is
/// no accessor anywhere in this workspace that returns one.
///
/// # `second` is what stops this node inventing coins
///
/// A second, independently configured endpoint and its URL, or `None` when the
/// caller decided there is nothing to hold this one to — that decision is
/// `NodeManager::second_source`'s and is argued there, not here. When it is
/// present, the funding node's outputs are held against it **before** anything
/// is selected, so the transaction that comes out is corroborated by
/// construction: `Corroborated` is handed to the builder in place of the chain,
/// and it is incapable of offering an outpoint the second node has never heard
/// of. Checking afterwards instead would leave a window where selection had
/// already picked a coin the check then had to refuse — and would have to
/// refuse the whole payment over one coin that is merely newer than the second
/// node.
///
/// It costs one extra `getaddressutxos` to each node: one to read what the
/// primary offers, one to ask the second whether it has them — plus one
/// `getblockcount` to the second, and only when the two answers differ, because
/// that is the only time its tip decides anything. Read
/// [`pecu_chain::corroborate`] for the comparison rule and for what this does
/// not cover.
///
/// # Both round trips happen outside `with_key`
///
/// The funding address is read from the vault's public side, which works while
/// the vault is locked, so the two network calls the check costs are made
/// before the key is decrypted rather than inside the window the signature
/// opens. That matters because the second endpoint is *someone else's*: a slow
/// one — or a deliberately slow one — could otherwise hold this wallet's
/// decrypted private key resident for the length of its own timeout, on demand.
/// The keystore treats the length of that window as a deliberate trade, and it
/// is not a trade to hand to a third party.
///
/// The address cannot drift from the key that signs. `Vault::with_key`
/// re-derives the address from the decrypted scalar and refuses if it does not
/// match the entry this one was read from, so either they are the same address
/// or nothing is signed at all.
pub fn prepare(
    chain: &Chain,
    second: Option<&Corroborator<'_>>,
    vault: &Vault,
    label: &str,
    draft: &SendDraft,
    name: &str,
) -> Result<Prepared, SendError> {
    let to = draft.to.trim();
    // No shielded guard here any more. This function builds the transparent
    // route and nothing else; the router in the core decides which builder a
    // draft reaches. A `zs…` arriving here would be a routing bug, and the
    // parse below reports it as the bad address it is from this builder's point
    // of view rather than inventing a second explanation.
    let destination = to.parse::<Address>().map_err(|_| SendError::BadAddress)?;

    // Whether the fee heuristic sizes every output at 200 bytes instead of 34,
    // decided the way `plan_transparent_send` decides it and for the same
    // reason: one identity recipient makes the whole transaction smart-output
    // priced. Working it out differently here would move the fee ladder by an
    // input and leave a send-all quietly short.
    let has_smart_outputs = destination.kind() == AddressKind::Identity;

    // Parsed before the key is opened, as it always was. A send-all has nothing
    // to parse — the field it would have been typed into is not on the form.
    let typed = if draft.send_all {
        None
    } else {
        let amount =
            Amount::from_coins_str(draft.amount.trim()).map_err(|_| SendError::BadAmount)?;
        if amount.is_zero() {
            return Err(SendError::NothingToSend);
        }
        Some(amount)
    };

    let from = funding_address(vault, label)?;

    // Asked before anything reads a coin, and before the key is opened: the
    // whole value of the answer is that it arrives before selection does.
    let vouched = match second {
        None => None,
        Some(secondary) => Some(corroborated_funding(chain, secondary, &from)?),
    };
    let (allowed, behind) = match vouched {
        Some(vouched) => (Some(vouched.allowed), vouched.behind),
        None => (None, None),
    };

    let built = vault.with_key(label, |key| match allowed {
        Some(allowed) => {
            let reader = pecu_chain::Corroborated::new(chain, &from, allowed);
            build_transparent(&reader, key, &from, to, typed, has_smart_outputs)
        }
        None => build_transparent(chain, key, &from, to, typed, has_smart_outputs),
    })?;
    let (unsent, amount) = match built {
        Ok(built) => built,
        Err(error) => {
            return Err(match (behind, second) {
                (Some(behind), Some(secondary)) => shortfall(error, behind, secondary.url),
                _ => error,
            })
        }
    };

    Ok(Prepared {
        signed: Signed::Transparent(unsent),
        to: to.to_string(),
        // The resolved figure, not the draft. `review` echoes this field for
        // `amount_display` rather than reading it back out of the bytes — only
        // the outputs, the fee and the change come from there — so a send-all
        // that left this alone would show a zero beside a correct outputs list.
        amount,
        route: pecu_protocol::Route::Transparent,
        spends: Vec::new(),
        name: name.to_string(),
        corroborated_by: second.map_or_else(String::new, |second| second.url.to_string()),
        withheld: behind.map_or(0, |behind| behind.count),
    })
}

/// Which address a key pays from, without opening the vault.
///
/// `Vault::keys` works while locked — it reads the public half of each entry —
/// so the corroboration round trips can happen before the key is decrypted.
/// The answer is not taken on trust: `with_key` derives the address from the
/// decrypted scalar and refuses the whole operation if it disagrees with the
/// entry, so a wrong answer here cannot become a signature over a different
/// address's coins.
fn funding_address(vault: &Vault, label: &str) -> Result<String, SendError> {
    vault
        .keys()
        .into_iter()
        .find(|key| key.label == label)
        .map(|key| key.address)
        .ok_or_else(|| SendError::Vault(VaultError::NoSuchKey(label.to_string())))
}

/// Say why a build ran out of coins when some were held back.
///
/// Without this the commonest honest disagreement — a second node one block
/// behind — reaches the send form as "Not enough spendable coins" beside a
/// balance the screen still shows in full, which is the same least-useful
/// sentence that made `Diverged` a distinct verdict in the first place.
/// Anything else the build objected to is its own answer and is passed
/// through: the coins being filtered does not make a bad address a funding
/// problem.
fn shortfall(error: SendError, behind: Behind, secondary: &str) -> SendError {
    match error {
        SendError::Flow(FlowError::InsufficientFunds { .. }) | SendError::NotEnoughForFee => {
            SpendRefused::SecondSourceBehind {
                count: behind.count,
                secondary: secondary.to_string(),
                tip: behind.tip,
            }
            .into()
        }
        other => other,
    }
}

/// Which outpoints at `address` a second node will vouch for, and what it set
/// aside.
///
/// One request to the primary, one or two to the secondary. The primary's
/// answer is read here rather than taken from a later `spendable`, because a
/// comparison wants both sides from the same moment — and because the build
/// below is then handed a filter rather than a verdict.
///
/// Every way this can fail is a refusal, never a silent pass. The SDK's own
/// second source puts the rule plainly and it is worth repeating at the site:
/// *"a source that cannot answer is a failure, not a pass … an uncorroborated
/// one silently substituted would make the whole thing decorative the first
/// time a node went down."* A caller reaching this function has already decided
/// corroboration is required.
///
/// The four verdicts get four different sentences, deliberately. "These two
/// nodes disagree", "the second one is behind", "the second one could not
/// answer" and "there is no second one" have four different remedies, and
/// merging any of them sends somebody after the wrong problem.
fn corroborated_funding<R: ChainReader>(
    reader: &R,
    secondary: &Corroborator<'_>,
    address: &str,
) -> Result<Vouched, SendError> {
    let offered = reader
        .address_utxos(&[address])
        .map_err(|error| SendError::Flow(error.into()))?;

    match corroborate::against(secondary.chain, address, &offered) {
        Corroboration::Agreed { .. } => Ok(Vouched {
            allowed: corroborate::agreed_outpoints(&offered, &[]),
            behind: None,
        }),
        // The common honest case: one node has indexed a block the other has
        // not, and the second node's own tip is the evidence. Spend the subset
        // both have and carry the count out, so the review can say what was
        // left behind rather than a send-all silently moving less than the
        // screen said.
        //
        // `kept` may be zero — one coin at the address, confirmed a minute ago,
        // is the commonest wallet shape there is — and that is still lag rather
        // than the attack. It gets its own refusal instead of being filtered to
        // nothing and reaching the form as "not enough funds".
        Corroboration::Lagging {
            withheld,
            kept,
            tip,
        } => {
            if kept == 0 {
                return Err(SpendRefused::SecondSourceBehind {
                    count: withheld.len(),
                    secondary: secondary.url.to_string(),
                    tip,
                }
                .into());
            }
            Ok(Vouched {
                behind: Some(Behind {
                    count: withheld.len(),
                    tip,
                }),
                allowed: corroborate::agreed_outpoints(&offered, &withheld),
            })
        }
        // The shape of the attack. Two chains' unspent outputs for one address
        // are disjoint, so a node serving another chain's coins lands here —
        // and so does one that mixes a real coin in with invented ones, which
        // is why this arm does not care what survived. Named, because filtering
        // to the empty set would surface as "not enough funds", the least
        // useful sentence available for the one case it would be describing.
        Corroboration::Diverged { unexplained, .. } => Err(SpendRefused::Uncorroborated {
            count: unexplained.len(),
            secondary: secondary.url.to_string(),
        }
        .into()),
        // Refused, and the reason goes to the log rather than into the
        // sentence. Which endpoint went silent is what a person can act on;
        // whether it timed out or refused the method outright is what somebody
        // debugging it needs, and the two want different words. A filtering
        // proxy that does not serve `getaddressutxos` at all makes this
        // permanent, which is exactly the state a log line has to explain.
        Corroboration::Unavailable { reason } => {
            tracing::warn!(
                secondary = %secondary.url,
                %reason,
                "the second source could not be asked about the funding address",
            );
            Err(SpendRefused::SecondSourceSilent {
                secondary: secondary.url.to_string(),
            }
            .into())
        }
    }
}

/// Resolve the amount if it was not typed, then build and sign.
///
/// Generic over the reader so that the corroborated and uncorroborated paths
/// run the **same** build. A second copy of this, filtered, is how the two
/// would come to disagree about the fee ladder.
///
/// It is still two `spendable` round trips on a send-all — `prepare_send` reads
/// the funding set again a moment later. Folding them together would mean
/// reimplementing `prepare_send` here, and a second copy of the build path is
/// the worse trade on the money path than three RPC calls. The set can change
/// in between; if it does the build still succeeds, because the resolved amount
/// is at most the new total, and the review shows whatever it actually left
/// behind.
fn build_transparent<R: ChainReader>(
    reader: &R,
    key: &verus_sdk::verus_keys::PrivateKey,
    from: &str,
    to: &str,
    typed: Option<Amount>,
    has_smart_outputs: bool,
) -> Result<(Unsent<Sent>, Amount), SendError> {
    let amount = if let Some(amount) = typed {
        amount
    } else {
        let funding = network::spendable(reader, from)?;
        resolve_send_all(&funding, has_smart_outputs)?
    };
    let unsent = network::prepare_send(reader, key, to, amount)?;
    Ok((unsent, amount))
}

// ── Reviewing ───────────────────────────────────────────────────────────────

/// Read the signed transaction back out, output by output.
///
/// This is the step that makes the review worth having. Everything below comes
/// from deserializing `unsent.hex` — the bytes that will be broadcast — rather
/// than from the draft. If coin selection put the change somewhere unexpected,
/// or the fee is not what anyone assumed, it shows up here.
pub fn review(
    ticket: u64,
    prepared: &Prepared,
    from: &str,
    spendable: Amount,
    known_recipient: bool,
) -> SendReviewVm {
    // Decoded from the bytes for every route. A shielded transaction has
    // transparent outputs too — a shield's change, an unshield's recipient —
    // and where it has none the list is correctly empty: a `z→z` puts nothing
    // on the transparent side, which is the whole point of it and is worth
    // showing as an absence rather than hiding.
    let outputs = decode_outputs(prepared.signed.hex(), from);
    let fee = prepared.signed.fee();
    let change = prepared.signed.change();

    // Total leaving the wallet: what the recipient gets plus the fee. Change is
    // not part of it — it comes back — which is exactly the arithmetic a review
    // exists to make visible.
    let total = prepared.amount.checked_add(fee).unwrap_or(prepared.amount);

    SendReviewVm {
        ticket,
        outputs,
        amount_display: coins(prepared.amount),
        fee_display: coins(fee),
        total_display: coins(total),
        change_display: coins(change),
        balance_after_display: coins(spendable.checked_sub(total).unwrap_or(Amount::ZERO)),
        from_address: from.to_string(),
        first_time_recipient: !known_recipient,
        recipient_name: prepared.name.clone(),
        corroboration: corroboration_note(prepared),
    }
}

/// What the review says about the second node, if there was one.
///
/// Three answers, and the empty one is a real answer rather than a missing one:
/// on a default install, and on the two routes that spend notes, nothing
/// corroborated these coins and the review must not imply otherwise. Where
/// something did, the endpoint is named — a guard nobody can see is a guard
/// nobody notices has stopped working — and where that endpoint had not reached
/// every output's block, the count comes with it, because a send-all that moves
/// less than the balance on screen owes an explanation.
fn corroboration_note(prepared: &Prepared) -> NoteVm {
    if prepared.corroborated_by.is_empty() {
        NoteVm::none()
    } else if prepared.withheld == 0 {
        NoteVm::with("send-corroborated", [prepared.corroborated_by.clone()])
    } else {
        NoteVm::with(
            "send-withheld",
            [
                prepared.withheld.to_string(),
                prepared.corroborated_by.clone(),
            ],
        )
    }
}

/// Every output of the signed transaction, decoded.
///
/// A script this build cannot read is reported as unreadable rather than
/// guessed at. "We could not read one of the outputs of the transaction you are
/// about to sign" is a sentence someone can act on; a blank line is not.
pub(crate) fn decode_outputs(hex: &str, from: &str) -> Vec<ReviewOutputVm> {
    let Ok(bytes) = hex::decode(hex) else {
        return Vec::new();
    };
    let Ok(tx) = verus_sdk::verus_wire::TxV4::deserialize(&bytes) else {
        return Vec::new();
    };

    tx.outputs
        .iter()
        .map(|output| {
            let amount = Amount::from_sat(output.value);
            let (address, kind) = describe(&output.script_pubkey);

            ReviewOutputVm {
                // Change is not a label the builder attaches — it is the output
                // that pays us back, recognised by address.
                //
                // **Except a conversion.** Its decoded address is the delivery
                // destination out of the payload, and this wallet converts to
                // itself — so an address comparison alone labels the value on
                // its way out "Change · back to your wallet" and draws it in the
                // quiet style meant for the output that does not matter. It is
                // the one that does. Caught by
                // `the_conversion_output_is_named_rather_than_reported_as_unreadable`,
                // which is worth more than it sounds: the mistake renders as a
                // perfectly plausible screen.
                is_change: address.as_deref() == Some(from) && kind.code != "output-conversion",
                address,
                kind,
                amount_display: coins(amount),
            }
        })
        .collect()
}

fn describe(script: &[u8]) -> (Option<String>, NoteVm) {
    use verus_sdk::decode::OutputKind;

    match verus_sdk::decode::decode_output_script(script) {
        Ok(OutputKind::PubKeyHash { hash }) => (
            Some(Address::new(AddressKind::PubKeyHash, hash).to_string()),
            NoteVm::plain("output-payment"),
        ),
        Ok(OutputKind::PubKey { hash, .. }) => (
            Some(Address::new(AddressKind::PubKeyHash, hash).to_string()),
            NoteVm::plain("output-to-public-key"),
        ),
        Ok(OutputKind::IdentityPayment { identity }) => (
            Some(Address::new(AddressKind::Identity, identity).to_string()),
            NoteVm::plain("output-to-verusid"),
        ),
        Ok(OutputKind::ReserveOutput { destination, .. }) => (
            destination_address(&destination),
            NoteVm::plain("output-token"),
        ),
        // A conversion in flight. The address the *script* pays is a protocol
        // constant — `RESERVE_TRANSFER_ADDRESS`, which is nobody — so the
        // destination shown is the one inside the payload, which is where the
        // converted value is actually delivered. Naming the holder here would
        // put an address on the review that means nothing and belongs to no
        // one.
        Ok(OutputKind::ReserveTransfer { transfer, .. }) => (
            destination_address(&transfer.destination.recipient),
            NoteVm::plain("output-conversion"),
        ),
        Ok(other) => (
            None,
            NoteVm::with("output-unrecognised", [format!("{other:?}")]),
        ),
        // Deliberately loud. An unreadable output in a transaction about to be
        // signed is the one thing a review must not present as ordinary.
        Err(_) => (None, NoteVm::plain("output-unreadable")),
    }
}

fn destination_address(destination: &verus_sdk::decode::Destination) -> Option<String> {
    use verus_sdk::decode::Destination;

    match destination {
        Destination::PubKeyHash(hash) => {
            Some(Address::new(AddressKind::PubKeyHash, *hash).to_string())
        }
        Destination::Identity(hash) => Some(Address::new(AddressKind::Identity, *hash).to_string()),
        _ => None,
    }
}

/// What a conversion's reserve-transfer output actually says, read back out of
/// the signed bytes.
///
/// Everything about a conversion is inside one CryptoCondition payload: the
/// currency, the amount, the fee and the delivery address. A review that did
/// not open it would be showing the form back to the person who filled it in.
pub(crate) struct Conversion {
    /// How much of the source currency is being moved, in its smallest unit.
    pub amount_sats: u64,
    /// The transfer fee written into the payload.
    pub fee_sats: u64,
    /// The native value the output itself carries.
    ///
    /// Amount plus fee when the source is the chain's own currency; the fee
    /// alone when it is a token, because a token's value travels in the payload
    /// rather than in satoshis. Taken from the output rather than worked out,
    /// so the review reports what was signed and not what should have been.
    pub native_sats: u64,
    /// Where the converted value is delivered. `None` for a destination shape
    /// this build does not decode, which is reported rather than guessed at.
    pub recipient: Option<String>,
}

/// Find the conversion in a signed transaction, if there is one.
///
/// One, not a list: everything this wallet builds carries exactly one reserve
/// transfer. The first is taken and any second would be ignored — which is not
/// a silent truncation, because a transaction with two of them is not something
/// this application can produce, and `decode_outputs` shows every output
/// regardless.
pub(crate) fn conversion_in(hex: &str) -> Option<Conversion> {
    use verus_sdk::decode::OutputKind;

    let bytes = hex::decode(hex).ok()?;
    let tx = verus_sdk::verus_wire::TxV4::deserialize(&bytes).ok()?;

    tx.outputs.iter().find_map(|output| {
        let OutputKind::ReserveTransfer { transfer, .. } =
            verus_sdk::decode::decode_output_script(&output.script_pubkey).ok()?
        else {
            return None;
        };
        Some(Conversion {
            // The payload's `CTokenOutput` carries the source currency and what
            // is being moved. Summed rather than indexed: the shape is one pair
            // for everything built here, and a `[0]` would panic on a
            // transaction that was not.
            amount_sats: transfer.tokens.iter().map(|(_, amount)| amount).sum(),
            fee_sats: transfer.fees,
            native_sats: output.value,
            recipient: destination_address(&transfer.destination.recipient),
        })
    })
}

// ── Sending ─────────────────────────────────────────────────────────────────

/// Hand the bytes to a node. **Blocking, and not cancellable.**
///
/// Takes a [`SpendPermit`] because [`Chain::broadcaster`] does, and that is the
/// entire spending guard: the permit cannot be constructed outside
/// `pecu_chain::permit`, and there is no other route to a `Broadcaster` in
/// this application.
///
/// Abandoning this mid-flight would manufacture exactly the ambiguity
/// `BroadcastUncertain` exists to report, which is why nothing above it may
/// cancel it.
pub fn broadcast(
    chain: &Chain,
    permit: &SpendPermit,
    prepared: Prepared,
) -> Result<Sent, FlowError> {
    prepared.signed.broadcast(chain, permit)
}

/// Build a `t→z`: transparent coin into this wallet's shielded pool.
///
/// The proving parameters are loaded here, inside the worker, because that is
/// where the seconds can be spent. Whether they *exist* was settled before this
/// was ever dispatched — see [`SendError::ParamsMissing`].
/// # A shield is corroborated, and the shielded routes are not
///
/// It would be easy to read "touches the shielded pool" as "has no second
/// source" and skip the check here. That is wrong, and it is the wrong that
/// would have left the whole guard one character away from being bypassed: a
/// `t→z` has no input notes, no witnesses and no anchor to fetch. It is funded
/// by `shield::plan` from `network::spendable` at a transparent address —
/// `getaddressutxos` on the RPC node, byte for byte the set a transparent
/// payment reads. Typing a `zs…` into the same Send form instead of an `R…`
/// changes the outputs, not where the coins come from.
///
/// So the same `Corroborated` reader is handed to the planner, and the same
/// refusals apply. `prepare_shielded` — `z→z` and `z→t` — is the one that
/// really has nothing to ask, because its inputs are notes from a single
/// lightwalletd.
pub fn prepare_shield(
    reader: &impl verus_sdk::network::ChainReader,
    second: Option<&Corroborator<'_>>,
    vault: &Vault,
    label: &str,
    from_address: &str,
    draft: &SendDraft,
    located: &crate::params::Located,
) -> Result<Prepared, SendError> {
    let vouched = match second {
        None => None,
        Some(secondary) => Some(corroborated_funding(reader, secondary, from_address)?),
    };

    let attempt = match &vouched {
        Some(vouched) => {
            let filtered =
                pecu_chain::Corroborated::new(reader, from_address, vouched.allowed.clone());
            crate::shield::plan(&filtered, from_address, &draft.to, &draft.amount)
        }
        None => crate::shield::plan(reader, from_address, &draft.to, &draft.amount),
    };
    let behind = vouched.as_ref().and_then(|vouched| vouched.behind);
    let planned = match attempt {
        Ok(planned) => planned,
        Err(error) => {
            return Err(match (&error, behind) {
                // The same courtesy the transparent route gets, and it has to
                // be done on the shield's own error rather than through
                // `shortfall`: this route's shortfall is
                // `ShieldError::NotEnough`, which flattens to a sentence about
                // shielding rather than to the SDK's `InsufficientFunds`.
                (crate::shield::ShieldError::NotEnough { .. }, Some(behind)) => {
                    SpendRefused::SecondSourceBehind {
                        count: behind.count,
                        secondary: second.map_or_else(String::new, |second| second.url.to_string()),
                        tip: behind.tip,
                    }
                    .into()
                }
                _ => shield_error(error),
            })
        }
    };

    let params = crate::params::load(located).map_err(|_| SendError::ParamsMissing)?;
    let shield = crate::shield::prepare(vault, label, &params, &planned).map_err(shield_error)?;

    Ok(Prepared {
        to: shield.to.clone(),
        amount: shield.amount,
        route: pecu_protocol::Route::Shield,
        spends: Vec::new(),
        name: String::new(),
        corroborated_by: second.map_or_else(String::new, |second| second.url.to_string()),
        withheld: vouched
            .and_then(|vouched| vouched.behind)
            .map_or(0, |behind| behind.count),
        signed: Signed::Shield(shield),
    })
}

/// Build a `z→z` or a `z→t`: shielded notes out.
///
/// # Why the proving happens inside a closure
///
/// `with_shielded_key` hands over the extended spending key for one operation,
/// and proving *is* that operation — so tens of seconds are spent inside it.
/// That is a deliberate trade the keystore documents: a long window that closes
/// beats handing the key to a thread and never saying when it stops.
pub fn prepare_shielded<T: verus_sdk::light::LightTransport>(
    light: &verus_sdk::light::LightClient<T>,
    reader: &impl verus_sdk::network::ChainReader,
    vault: &Vault,
    label: &str,
    planned: &crate::shielded::PlannedSpend,
    located: &crate::params::Located,
) -> Result<Prepared, SendError> {
    let params = crate::params::load(located).map_err(|_| SendError::ParamsMissing)?;

    let unsent = vault
        .with_shielded_key(label, |extsk| {
            crate::shielded::prove_spend(light, reader, &params, extsk, planned)
        })?
        .map_err(shielded_error)?;

    let (to, route) = match &planned.to {
        crate::shielded::Destination::Shielded(address) => {
            (address.clone(), pecu_protocol::Route::Private)
        }
        crate::shielded::Destination::Transparent(address) => {
            (address.clone(), pecu_protocol::Route::Unshield)
        }
    };

    Ok(Prepared {
        signed: Signed::Shielded(unsent),
        to,
        amount: planned.amount,
        route,
        spends: planned.nullifiers(),
        name: String::new(),
        // Genuinely out of scope, and the only route that is: the inputs are
        // notes, and the notes, the witnesses and the anchor all come from one
        // lightwalletd with no second one to ask. Said as a blank rather than
        // as a claim — see `pecu_chain::corroborate`, and see `prepare_shield`
        // for why a `t→z` is *not* in this group.
        corroborated_by: String::new(),
        withheld: 0,
    })
}

/// Flatten a shield's own error into the send path's.
///
/// The wording is kept — a shield refuses for reasons a transparent send has no
/// vocabulary for, and replacing them with "could not send" would throw away
/// the only description of what went wrong.
fn shield_error(error: crate::shield::ShieldError) -> SendError {
    use crate::shield::ShieldError;
    match error {
        ShieldError::BadAddress => SendError::BadAddress,
        ShieldError::BadAmount => SendError::BadAmount,
        ShieldError::Params(_) => SendError::ParamsMissing,
        ShieldError::Vault(e) => SendError::Vault(e),
        ShieldError::Flow(e) => SendError::Flow(e),
        other => SendError::Shielded(other.to_string()),
    }
}

/// Flatten a shielded spend's error the same way.
fn shielded_error(error: crate::shielded::ShieldedError) -> SendError {
    use crate::shielded::ShieldedError;
    match error {
        ShieldedError::BadAddress => SendError::BadAddress,
        ShieldedError::BadAmount => SendError::BadAmount,
        other => SendError::Shielded(other.to_string()),
    }
}

/// Re-send bytes that were already signed, after an ambiguous failure.
///
/// **Takes hex, not a draft.** Rebuilding would select different coins and
/// produce a different transaction — and if the first one did land, that is a
/// second payment. This is the only resend path, and it cannot rebuild because
/// it is not given anything to rebuild from.
pub fn resend(
    chain: &Chain,
    permit: &SpendPermit,
    hex: &str,
    txid: &str,
) -> Result<String, FlowError> {
    network::broadcast(&chain.broadcaster(permit), hex, txid)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADDRESS: &str = "RQr2cUkF46n7y8WRzDkd1iV9gHusSSQuzX";
    /// A real shielded address, so the decode that decides the route succeeds
    /// for the reason it would in the product rather than by accident.
    const SHIELDED_ADDRESS: &str =
        "zs18pytujp8qu73a3fu6g9chl7mfumrr0htyqsh60r3ed4capagqwm8tx2l8f9c5g7w87q4566uph3";

    fn draft(to: &str, amount: &str) -> SendDraft {
        SendDraft {
            from_label: "main".to_string(),
            to: to.to_string(),
            amount: amount.to_string(),
            from_pool: pecu_protocol::Pool::Transparent,
            send_all: false,
        }
    }

    #[test]
    fn a_transparent_address_is_accepted_and_named() {
        let verdict = validate(&draft(ADDRESS, "1.5"), Amount::from_sat(1_000_000_000));
        assert!(verdict.to_valid);
        assert_eq!(verdict.to_note.code, "address-transparent");
        assert!(verdict.amount_valid);
        assert!(verdict.ready);
    }

    /// A shielded address is a destination like any other.
    ///
    /// It was refused for one commit, while the wallet could see shielded funds
    /// and not pay them, and the refusal said so in those words. Now it is
    /// accepted, and what the form reports instead is the **route** — because
    /// paying a `zs…` from the transparent balance is a different transaction
    /// from paying it from the shielded one, and the difference decides what
    /// the chain records.
    #[test]
    fn a_shielded_address_is_a_destination_and_names_the_route() {
        let from_transparent = validate(
            &draft(SHIELDED_ADDRESS, "1"),
            Amount::from_sat(1_000_000_000),
        );
        assert!(from_transparent.to_valid);
        assert_eq!(from_transparent.to_note.code, "address-shielded");
        assert!(from_transparent.ready);
        assert_eq!(from_transparent.route, pecu_protocol::Route::Shield);

        let mut shielded_source = draft(SHIELDED_ADDRESS, "1");
        shielded_source.from_pool = pecu_protocol::Pool::Shielded;
        assert_eq!(
            validate(&shielded_source, Amount::from_sat(1_000_000_000)).route,
            pecu_protocol::Route::Private,
        );
    }

    /// And paying a transparent address out of the shielded pool is the fourth
    /// route, not the first one.
    #[test]
    fn paying_transparently_from_the_shielded_pool_is_an_unshield() {
        let mut draft = draft(ADDRESS, "1");
        draft.from_pool = pecu_protocol::Pool::Shielded;

        let verdict = validate(&draft, Amount::from_sat(1_000_000_000));
        assert!(verdict.to_valid);
        assert_eq!(verdict.route, pecu_protocol::Route::Unshield);
        // Every route but the first needs the prover, `Shield` included: a
        // Sapling output needs a proof exactly as a spend does.
        assert!(verdict.route.needs_proving());
        assert!(!pecu_protocol::Route::Transparent.needs_proving());
    }

    /// Something that only looks like one is still a typo.
    ///
    /// The check decodes rather than matching a prefix, so a `zs1…` that fails
    /// its checksum must fall through to the ordinary refusal — otherwise a
    /// mistyped shielded address would be explained as an unbuilt feature and
    /// nobody would look at the characters.
    #[test]
    fn a_broken_shielded_address_is_still_a_typo() {
        let verdict = validate(&draft("zs1nonsense", "1"), Amount::from_sat(1_000_000_000));
        assert!(!verdict.to_valid);
        assert_eq!(verdict.to_note.code, "address-unparsable");
    }

    #[test]
    fn nonsense_is_refused_with_a_reason_rather_than_a_cross() {
        let verdict = validate(&draft("not-an-address", "1"), Amount::from_sat(100_000_000));
        assert!(!verdict.to_valid);
        // The code, not the wording. What the core decides is *which* refusal
        // this is; the sentence belongs to the interface and can be rewritten
        // or translated without this test having an opinion about it.
        assert_eq!(verdict.to_note.code, "address-unparsable");
        assert!(!verdict.ready);
    }

    /// A Bitcoin address is valid base58 for a chain this wallet does not
    /// speak, and paying it would burn the coins.
    #[test]
    fn an_address_from_another_chain_is_refused() {
        let verdict = validate(
            &draft("1BvBMSEYstWetqTFn5Au4m4GFg7xJaNVN2", "1"),
            Amount::from_sat(100_000_000),
        );
        assert!(!verdict.to_valid);
    }

    #[test]
    fn an_amount_above_the_balance_says_so() {
        let verdict = validate(&draft(ADDRESS, "10"), Amount::from_sat(100_000_000));
        assert!(!verdict.amount_valid);
        assert_eq!(verdict.amount_note.code, "amount-above-spendable");
        // The figure still travels with it, and still spelled by the core —
        // money is formatted in one place and this test is what says so.
        assert_eq!(verdict.amount_note.args, vec!["1.0000 0000".to_string()]);
        assert!(!verdict.ready);
    }

    #[test]
    fn zero_is_not_an_amount() {
        let verdict = validate(&draft(ADDRESS, "0"), Amount::from_sat(100_000_000));
        assert!(!verdict.amount_valid);
        assert!(!verdict.ready);
    }

    /// Nine decimal places is not a rounding problem to be solved quietly.
    #[test]
    fn more_precision_than_a_satoshi_is_refused() {
        let verdict = validate(&draft(ADDRESS, "1.234567891"), Amount::from_sat(u64::MAX));
        assert!(!verdict.amount_valid);
        assert_eq!(verdict.amount_note.code, "amount-unparsable");
    }

    /// An empty field is not an error yet — it is a field nobody has filled in.
    #[test]
    fn an_empty_form_is_not_yet_wrong() {
        let verdict = validate(&draft("", ""), Amount::ZERO);
        assert!(verdict.to_note.code.is_empty());
        assert!(verdict.amount_note.code.is_empty());
        assert!(!verdict.ready);
    }

    /// The amount field is not on the form while this is set, so there is
    /// nothing to be empty and nothing to be above the balance.
    #[test]
    fn a_draft_that_sends_everything_needs_no_typed_amount() {
        let mut draft = draft(ADDRESS, "");
        draft.send_all = true;

        let verdict = validate(&draft, Amount::from_sat(100_000_000));
        assert!(verdict.amount_valid);
        assert!(verdict.amount_note.code.is_empty());
        assert!(verdict.ready);
    }

    /// A stale string left in `amount` by a form that has since switched modes
    /// must not change the verdict either way.
    #[test]
    fn a_draft_that_sends_everything_ignores_whatever_was_typed_before() {
        let mut draft = draft(ADDRESS, "99999999");
        draft.send_all = true;

        let verdict = validate(&draft, Amount::from_sat(100_000_000));
        assert!(verdict.amount_valid);
        assert!(verdict.ready);
    }

    /// Knowable without the chain, and worth saying before the button is
    /// pressed rather than after a build has been refused.
    #[test]
    fn a_draft_that_sends_everything_from_a_balance_below_the_fee_says_so() {
        let mut draft = draft(ADDRESS, "");
        draft.send_all = true;

        let verdict = validate(&draft, Amount::from_sat(5_000));
        assert!(!verdict.amount_valid);
        assert_eq!(verdict.amount_note.code, "amount-below-fee");
        // No figure travels with it. The fee is not known until coins have been
        // selected, and a quoted guess is the thing the amount field was taken
        // off the form to avoid.
        assert!(verdict.amount_note.args.is_empty());
        assert!(!verdict.ready);
    }

    /// The flag rides on the same draft as the pool selector, so it reaches the
    /// shielded routes whatever the form offers. Refused rather than ignored.
    #[test]
    fn sending_everything_out_of_the_shielded_pool_is_refused_by_the_form() {
        let mut draft = draft(ADDRESS, "");
        draft.send_all = true;
        draft.from_pool = pecu_protocol::Pool::Shielded;

        let verdict = validate(&draft, Amount::from_sat(100_000_000));
        assert_eq!(verdict.route, pecu_protocol::Route::Unshield);
        assert!(!verdict.amount_valid);
        assert_eq!(verdict.amount_note.code, "send-all-transparent-only");
        assert!(!verdict.ready);
    }

    /// The toggle is drawn whenever the transparent balance is paying, and a
    /// shielded recipient does not turn it off — so this is reachable by
    /// pasting a `zs1…` into a form that is already set to send everything.
    ///
    /// It said `ready` here once, and then `Core::refuses_send_all` turned the
    /// same draft away: the form branched on the pool and the core on the
    /// route, and `R → z` is the one combination where those two disagree.
    #[test]
    fn sending_everything_to_a_shielded_address_is_refused_by_the_form() {
        let mut draft = draft(SHIELDED_ADDRESS, "");
        draft.send_all = true;

        let verdict = validate(&draft, Amount::from_sat(100_000_000));
        assert_eq!(verdict.route, pecu_protocol::Route::Shield);
        assert!(!verdict.amount_valid);
        assert!(!verdict.ready);
    }

    /// Not the same sentence as the shielded *source*, and that is the point of
    /// having two codes: on this route the money genuinely is coming out of the
    /// transparent balance, so "choose an amount to pay from the shielded one"
    /// would be telling somebody to correct the half that is already right.
    #[test]
    fn the_two_send_all_refusals_do_not_share_a_sentence() {
        let mut from_shielded = draft(ADDRESS, "");
        from_shielded.send_all = true;
        from_shielded.from_pool = pecu_protocol::Pool::Shielded;

        let mut to_shielded = draft(SHIELDED_ADDRESS, "");
        to_shielded.send_all = true;

        let balance = Amount::from_sat(100_000_000);
        assert_ne!(
            validate(&from_shielded, balance).amount_note.code,
            validate(&to_shielded, balance).amount_note.code,
        );
    }

    /// Garbage in must not panic: this runs on every keystroke.
    #[test]
    fn a_transaction_that_cannot_be_decoded_yields_no_outputs() {
        assert!(decode_outputs("not hex", ADDRESS).is_empty());
        assert!(decode_outputs("deadbeef", ADDRESS).is_empty());
        assert!(decode_outputs("", ADDRESS).is_empty());
    }
}
