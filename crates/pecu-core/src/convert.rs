//! Turning one currency into another.
//!
//! # A conversion is a request at an unknown price
//!
//! The SDK says this at the top of `verus_flows::convert` and it decides the
//! shape of everything here. The transaction says what goes in and where the
//! result should land; it says nothing about what comes out. The chain performs
//! the conversion when it *imports* the output, at whatever the reserve ratios
//! are then — a block later at best. There is no slippage bound in the
//! protocol, and no floor anybody records is enforced by anything.
//!
//! So every figure this module produces is an estimate, and the one place that
//! is not hedged — `minimum` — is a record of intent rather than a promise.
//!
//! # Why this asks the node where `market` divides
//!
//! Two questions that look like one. `market::Book` derives a **mid price** by
//! dividing two reserve figures: what a currency is worth, before anything
//! moves. `estimateconversion` answers what *this* conversion, at *this* size,
//! is expected to yield after the pool has been moved by it and the fees have
//! been taken. A wallet that quoted the first would be quoting a price it
//! cannot honour, and the gap between them is the number a person actually
//! needs — which is why both are here and why [`slippage`] is the difference.
//!
//! # One pool, or no conversion
//!
//! A conversion runs through a single fractional currency holding both sides.
//! Two currencies sharing no pool need two transactions with the intermediate
//! held in between, which is a different thing to agree to — so this refuses
//! rather than quietly planning it. See [`market::Book::route`].

use std::collections::BTreeMap;

use pecu_chain::{Chain, SpendPermit};
use pecu_keystore::{Vault, VaultError};
use pecu_protocol::{format, ConvertDraft, ConvertQuoteVm, NoteVm};
use verus_sdk::convert::ConversionKind;
use verus_sdk::currency::CurrencyId;
use verus_sdk::money::{Amount, Utxo};
use verus_sdk::network::{self, FlowError, Sent, Unsent};
use verus_sdk::verus_keys::{Address, AddressKind};

use crate::market::{Book, Pool, UNKNOWN};

/// Above this, the quote is worth a second look.
const SLIPPAGE_WARN: f64 = 0.01;
/// Above this, it is worth refusing to be quiet about.
const SLIPPAGE_LOUD: f64 = 0.03;
/// The same figure, in per-mille.
///
/// Two spellings of one number because they are used in two arithmetics: the
/// tone compares floats, and the floor is satoshis — an integer count that must
/// not go near a float on its way to a figure somebody agrees to.
/// `the_two_spellings_of_the_loud_threshold_agree` holds them together.
const FLOOR_ROOM_PERMILLE: u64 = 30;

/// Satoshis as coins, for a ratio.
///
/// # Why a cast is allowed here and nowhere else in this workspace
///
/// Because the result is divided by another one of these and the quotient is a
/// price, not an amount. Nothing built from this reaches a transaction: the
/// figures a person agrees to — `pay`, `get`, `minimum` — are all formatted from
/// the `Amount` itself by [`format::coins_u64`], which never sees a float.
///
/// The precision that is lost starts at 2^53 satoshis, which is ninety million
/// coins. A wallet holding that much has a different problem.
#[allow(clippy::cast_precision_loss)]
fn coins(amount: Amount) -> f64 {
    amount.to_sat() as f64 / pecu_protocol::SATS_PER_COIN as f64
}

/// A draft that survived every check that does not need a node.
#[derive(Clone, Debug, PartialEq)]
pub struct Ready {
    pub from: String,
    pub to: String,
    pub amount: Amount,
    /// The pool the conversion goes through, as the chain spells it.
    pub pool: String,
    /// The same pool by i-address.
    ///
    /// Both, because the two are asked for by different halves: the node is
    /// told the name — that is what turns up in its log — and the transaction
    /// is built from the id, which is what a `ConversionKind` is made of. See
    /// [`kind_for`], which is the one place the difference between the three
    /// kinds of conversion is decided, and decides it by comparing this
    /// against the two legs.
    pub pool_id: String,
    /// What `estimateconversion` should be told to route through, or `None`
    /// when one side **is** the pool and there is nothing to route via.
    pub via: Option<String>,
    /// What the reserves say one `from` is worth in `to`, before this
    /// conversion moves them. The baseline [`slippage`] measures against.
    pub mid: Option<f64>,
}

/// Everything that can be decided without asking anything.
///
/// Runs on every keystroke, so it touches nothing but memory: the book the
/// wallet already has, the balances it already read, and the text as typed.
///
/// # Errors
///
/// A named reason, for `note.slint` to put words to.
pub fn check(
    draft: &ConvertDraft,
    book: &Book,
    holdings: &BTreeMap<String, Amount>,
    names: &BTreeMap<String, String>,
) -> Result<Ready, NoteVm> {
    if draft.from.is_empty() || draft.to.is_empty() {
        return Err(NoteVm::plain("convert-pick-both"));
    }
    if draft.from == draft.to {
        return Err(NoteVm::plain("convert-same-currency"));
    }

    let Some(pool) = book.route(&draft.from, &draft.to) else {
        return Err(NoteVm::with(
            "convert-no-route",
            [name_of(names, &draft.from), name_of(names, &draft.to)],
        ));
    };

    let typed = draft.pay.trim();
    if typed.is_empty() {
        return Err(NoteVm::none());
    }
    let Ok(amount) = Amount::from_coins_str(typed) else {
        return Err(NoteVm::plain("amount-unparsable"));
    };
    if amount.is_zero() {
        return Err(NoteVm::plain("amount-zero"));
    }

    // What the wallet actually holds of the paying side. Absent is zero here
    // and only here: a currency with no entry is one no output paid us in,
    // which is a balance of nothing rather than an unknown.
    let held = holdings.get(&draft.from).copied().unwrap_or(Amount::ZERO);
    if amount > held {
        return Err(NoteVm::with(
            "convert-above-holding",
            [
                format::coins_u64(held.to_sat()),
                name_of(names, &draft.from),
            ],
        ));
    }

    Ok(Ready {
        from: draft.from.clone(),
        to: draft.to.clone(),
        amount,
        pool: pool.name.clone(),
        pool_id: pool.id.clone(),
        via: routed_via(pool, &draft.from, &draft.to),
        mid: pool.price(&draft.from, &draft.to),
    })
}

/// What `estimateconversion` needs told, and what it does not.
///
/// `via` names the fractional currency to route through, and it is **wrong** to
/// send it when one side is that currency: converting a reserve into the basket
/// itself is a direct conversion, and naming the destination as the route asks
/// the node to go through the thing it is going to.
fn routed_via(pool: &Pool, from: &str, to: &str) -> Option<String> {
    (pool.id != from && pool.id != to).then(|| pool.name.clone())
}

/// How far an estimate has fallen from the mid price, as a fraction.
///
/// `None` when there is no mid to measure against. Negative is possible and is
/// left signed: a conversion that yields *more* than the reserves imply is a
/// real thing on a pool somebody has moved in the other direction, and hiding
/// it behind an absolute value would report it as a cost.
pub fn slippage(estimated: Amount, mid_out: Option<f64>) -> Option<f64> {
    let mid = mid_out?;
    if !mid.is_finite() || mid <= 0.0 {
        return None;
    }
    let got = coins(estimated);
    Some((mid - got) / mid)
}

/// "positive" · "warning" · "negative", by the thresholds above.
///
/// Carried to the interface rather than compared there. Where the line sits is
/// a rule about money, and a rule about money that lives in a `.slint` binding
/// is a rule nothing can test.
pub fn tone(slippage: Option<f64>) -> &'static str {
    match slippage {
        Some(fraction) if fraction > SLIPPAGE_LOUD => "negative",
        Some(fraction) if fraction > SLIPPAGE_WARN => "warning",
        Some(_) => "positive",
        None => "unknown",
    }
}

/// The finished quote, once the node has answered.
///
/// `estimated` is what it expects out; `conversion_fee` is what it said the
/// conversion costs, when it said anything; `network_fee` is what a
/// transaction is expected to cost, which is a different fee paid to different
/// people and is never folded into the first.
pub fn quote(
    ready: &Ready,
    names: &BTreeMap<String, String>,
    holdings: &BTreeMap<String, Amount>,
    estimated: Amount,
    conversion_fee: Option<Amount>,
    network_fee: Option<Amount>,
) -> ConvertQuoteVm {
    let from = name_of(names, &ready.from);
    let to = name_of(names, &ready.to);

    let paid = coins(ready.amount);
    let got = coins(estimated);
    let mid_out = ready.mid.map(|mid| mid * paid);
    let drop = slippage(estimated, mid_out);

    let floor = floor(estimated).to_sat();

    ConvertQuoteVm {
        rate: if paid > 0.0 {
            format!("1 {from} = {} {to}", format::price(got / paid))
        } else {
            String::new()
        },
        from: from.clone(),
        to: to.clone(),
        pay: format::coins_u64(ready.amount.to_sat()),
        from_balance: held(holdings, &ready.from),
        // Without the currency, unlike the fees below: this one is read inside
        // the receiving box, which carries the name on the button beside it.
        // Repeating it there gives "133.5245 8143 DAI.vETH   DAI.vETH".
        get: format::coins_u64(estimated.to_sat()),
        via: ready.via.clone().unwrap_or_default(),
        // With the currency, because these three are in two different ones and
        // the lines they sit on do not say which. The conversion fee and the
        // network fee are taken from what goes in; the floor is what comes out.
        // A column of bare numbers here would read as one currency.
        conversion_fee: conversion_fee.map_or_else(
            || UNKNOWN.to_string(),
            |fee| format!("{} {from}", format::coins_u64(fee.to_sat())),
        ),
        network_fee: network_fee.map_or_else(
            || UNKNOWN.to_string(),
            |fee| format!("{} {from}", format::coins_u64(fee.to_sat())),
        ),
        minimum: format!("{} {to}", format::coins_u64(floor)),
        slippage: drop.map_or_else(
            || UNKNOWN.to_string(),
            |fraction| format!("{:.2}%", fraction * 100.0),
        ),
        slippage_tone: tone(drop).to_string(),
        // Said in words from the **warning** threshold, not only from the loud
        // one. A percentage in orange with no sentence beside it tells somebody
        // that something is wrong and not what — and "2.15%" is not a figure
        // anybody has an intuition for. The colour carries the severity; the
        // sentence carries the reason, and it is the same reason at both.
        note: match drop {
            Some(fraction) if fraction > SLIPPAGE_WARN => NoteVm::with(
                "convert-slippage-high",
                [format!("{:.2}%", fraction * 100.0)],
            ),
            _ => NoteVm::none(),
        },
        ready: true,
    }
}

/// The least a conversion is worth doing at, from what the node expects.
///
/// Not a protocol bound — see the module docs, and
/// [`ConvertReviewVm::minimum_display`] — so it is derived from the estimate
/// itself rather than from a preference nobody has been asked for: what the
/// node expects, less the room the loud slippage threshold allows.
///
/// A function rather than an expression inside [`quote`] because two sides need
/// the same number and must not compute it twice. The quote prints it, and the
/// core hands it to [`prepare`] as the floor that gets checked before signing —
/// so a second copy of this arithmetic would be a floor somebody was shown and
/// a different floor that was enforced.
pub fn floor(estimated: Amount) -> Amount {
    Amount::from_sat(
        estimated
            .to_sat()
            .saturating_mul(1000 - FLOOR_ROOM_PERMILLE)
            / 1000,
    )
}

/// A quote that says only why it is not one.
pub fn refused(
    draft: &ConvertDraft,
    names: &BTreeMap<String, String>,
    holdings: &BTreeMap<String, Amount>,
    note: NoteVm,
) -> ConvertQuoteVm {
    ConvertQuoteVm {
        from: name_of(names, &draft.from),
        to: name_of(names, &draft.to),
        pay: draft.pay.trim().to_string(),
        from_balance: held(holdings, &draft.from),
        get: String::new(),
        via: String::new(),
        rate: String::new(),
        conversion_fee: String::new(),
        network_fee: String::new(),
        minimum: String::new(),
        slippage: String::new(),
        slippage_tone: "unknown".to_string(),
        note,
        ready: false,
    }
}

/// What the wallet holds of one currency, spelled. Empty when none is chosen.
///
/// Zero rather than blank for a currency with no entry: the read succeeded and
/// found nothing, which is a balance. A currency nobody has chosen is the only
/// case with nothing to say.
fn held(holdings: &BTreeMap<String, Amount>, address: &str) -> String {
    if address.is_empty() {
        return String::new();
    }
    format::coins_u64(
        holdings
            .get(address)
            .copied()
            .unwrap_or(Amount::ZERO)
            .to_sat(),
    )
}

/// The name a currency is known by, or its i-address when it is not.
///
/// Never a guess. A currency the catalog has not heard of keeps the string that
/// identifies it, which is ugly and cannot be wrong.
fn name_of(names: &BTreeMap<String, String>, address: &str) -> String {
    if address.is_empty() {
        return String::new();
    }
    names
        .get(address)
        .cloned()
        .unwrap_or_else(|| address.to_string())
}

// ── Signing ─────────────────────────────────────────────────────────────────

/// The fee written into the reserve transfer, in the chain's own currency.
///
/// Not derived, and there is nothing here to derive it from. The SDK takes this
/// as a parameter and computes nothing; `estimateconversion`'s `fee` is a
/// different number — what the *pool* charges, denominated in the source
/// currency, already on its own line in the quote.
///
/// 20 010 satoshis is the figure every conversion in the SDK uses, including
/// `a_conversion_reaches_the_chain`, which builds against a live node and
/// asserts the transaction confirmed. A fee VRSCTEST accepted is the only
/// evidence available, and it is the whole reason for this value rather than a
/// rounder one. Named here so the next person to touch it finds the provenance
/// instead of a magic number.
const TRANSFER_FEE_SATS: u64 = 20_010;

#[derive(Debug, thiserror::Error)]
pub enum ConvertError {
    /// A conversion pays a bare key hash, always — see [`prepare`].
    #[error("a conversion has to be delivered to an R-address")]
    BadRecipient,
    #[error("that is not a currency this wallet can name")]
    BadCurrency,
    /// The node's estimate had already fallen below the floor by the time this
    /// was planned. Nothing was signed.
    #[error("the node now expects {expected}, and {floor} was the least this was worth doing at")]
    BelowFloor { expected: String, floor: String },
    /// The wallet holds the token in outputs it cannot use for this.
    #[error("this wallet cannot spend the {0} it holds in one conversion")]
    UnusableTokens(String),
    #[error(transparent)]
    Vault(#[from] VaultError),
    #[error(transparent)]
    Flow(#[from] FlowError),
    /// A read that failed on its own terms rather than inside a flow — asking
    /// the chain which currency is its own, which is how the token side finds
    /// out whether it has a token side at all.
    #[error(transparent)]
    Read(#[from] verus_sdk::network::RpcError),
}

/// A signed conversion that has not been sent, and what it is for.
///
/// **Never leaves the core**, for the reason [`crate::send::Prepared`] gives:
/// the interface gets a view model and a ticket number, and an interface that
/// held the signed hex would be one that could be made to send it.
pub struct Prepared {
    pub unsent: Unsent<Sent>,
    /// The two currencies, by i-address and by name. Both, because the review
    /// shows the name and the outputs decode to ids.
    pub from: String,
    pub to: String,
    pub from_name: String,
    pub to_name: String,
    /// What goes in.
    pub amount: Amount,
    /// What the node expected out at the moment this was planned.
    pub estimated: Amount,
    /// The floor that was checked against it. See [`ConvertReviewVm::minimum_display`].
    pub floor: Amount,
    /// The basket routed through, spelled, or empty when the conversion is
    /// direct.
    pub via: String,
}

/// What the chain is being asked to do, worked out from the route.
///
/// The book already found the single pool that holds both sides — see
/// [`Book::route`] — and which of the three conversions this is falls straight
/// out of where that pool sits relative to the two legs:
///
/// * the pool **is** what is being bought → a reserve into its fractional;
/// * the pool **is** what is being paid → a fractional back into a reserve;
/// * otherwise → one reserve into another, through the pool.
///
/// [`ConversionKind::Preconvert`] is deliberately unreachable. It applies only
/// to a currency that has not launched, and `Book::route` filters unstarted
/// pools out before anything gets here — so this wallet cannot accidentally
/// build one, which matters because the chain rejects a preconvert and an
/// ordinary conversion at opposite sides of the same block height.
///
/// # Errors
///
/// [`ConvertError::BadCurrency`] for anything that is not an i-address. The
/// legs come from the picker, which is built from the chain's own list, so this
/// is a guard rather than an expected outcome.
pub fn kind_for(pool_id: &str, from: &str, to: &str) -> Result<ConversionKind, ConvertError> {
    let target = currency_id(to)?;
    if pool_id == to {
        Ok(ConversionKind::IntoFractional { fractional: target })
    } else if pool_id == from {
        Ok(ConversionKind::IntoReserve { reserve: target })
    } else {
        Ok(ConversionKind::ReserveToReserve {
            via: currency_id(pool_id)?,
            target,
        })
    }
}

/// An i-address as the twenty bytes a transaction is built from.
fn currency_id(address: &str) -> Result<CurrencyId, ConvertError> {
    let parsed: Address = address.parse().map_err(|_| ConvertError::BadCurrency)?;
    Ok(CurrencyId::from_bytes(parsed.hash()))
}

/// Plan a conversion, then build and sign it. **Blocking.**
///
/// Two steps rather than one, and the order is the safety property.
/// `plan_conversion` takes a [`ChainReader`](verus_sdk::network::ChainReader)
/// and no `Broadcaster`, so it is *incapable* of sending — and it is where
/// every refusal that does not need a key happens: an unroutable pair, a
/// destination that is not an R-address, an estimate that has already fallen
/// through the floor. All of that is settled before the vault is opened, so a
/// conversion that cannot be made never causes a key to exist in memory at all.
///
/// Only then does `prepare_conversion` sign, inside `with_key` — the build, the
/// signature and the key's drop all happen in that closure, and there is no
/// accessor in this workspace that returns one.
///
/// # Why the recipient is checked here as well as in the SDK
///
/// Because the SDK's check is the last line of defence and this is the first.
/// `build_conversion` writes the destination as a **key hash, unconditionally**,
/// so an identity address does not pay that identity — it pays the R-form of
/// the same twenty bytes, which nobody holds a key for, and the value is gone.
/// This wallet always converts to its own active address, so the case should be
/// impossible; a guard that costs one comparison is worth having on the path
/// where "should be impossible" and "money is gone" are the same sentence.
///
/// # Errors
///
/// Everything the node refuses, plus the four above. Nothing partial: either a
/// signed transaction comes back or nothing was built.
pub fn prepare(
    chain: &Chain,
    vault: &Vault,
    label: &str,
    ready: &Ready,
    names: &BTreeMap<String, String>,
    recipient: &str,
    floor: Amount,
) -> Result<Prepared, ConvertError> {
    let kind = kind_for(&ready.pool_id, &ready.from, &ready.to)?;

    let refund: Address = recipient.parse().map_err(|_| ConvertError::BadRecipient)?;
    if refund.kind() != AddressKind::PubKeyHash {
        return Err(ConvertError::BadRecipient);
    }

    let fee = Amount::from_sat(TRANSFER_FEE_SATS);

    // Priced again here, rather than reusing the quote the form is showing.
    //
    // The quote is as old as the last keystroke, and the floor is checked once
    // — here — and never again by anything. A review built on a minute-old
    // number would be showing a floor that was tested against a price nobody
    // has looked at since.
    let plan = network::plan_conversion(
        chain,
        &ready.from,
        ready.amount,
        kind.clone(),
        recipient,
        refund,
        fee,
        Some(floor),
    )?;
    if !plan.acceptable() {
        return Err(ConvertError::BelowFloor {
            expected: crate::portfolio::coins(plan.estimated_out),
            floor: crate::portfolio::coins(floor),
        });
    }

    let token_funding = token_inputs(chain, recipient, &ready.from)?;

    let unsent = vault.with_key(label, |key| {
        network::prepare_conversion(
            chain,
            key,
            &ready.from,
            ready.amount,
            kind,
            recipient,
            fee,
            Some(floor),
            &token_funding,
        )
    })??;

    Ok(Prepared {
        unsent,
        from: ready.from.clone(),
        to: ready.to.clone(),
        from_name: name_of(names, &ready.from),
        to_name: name_of(names, &ready.to),
        amount: ready.amount,
        estimated: plan.estimated_out,
        floor,
        via: ready.via.clone().unwrap_or_default(),
    })
}

/// The token-bearing outputs a conversion of `source` has to spend.
///
/// Empty when the source is the chain's own currency, which is not a shortcut:
/// the builder **refuses** token inputs on a native conversion, because a
/// native conversion's value travels in the output's satoshis and a token input
/// there would be value it could not account for.
///
/// # Why every matching output goes in, and why they are filtered so narrowly
///
/// Every token input is spent whole and the surplus comes back as change, so
/// including more than the conversion needs costs nothing — while leaving one
/// out that the builder then has to balance against is how a token gets
/// destroyed. So: all of them, of exactly this currency.
///
/// "Exactly" is the builder's rule, not a preference. A reserve output may
/// carry several currencies at once, and it refuses one that does — the others
/// would have no change output and would simply cease to exist. Such an output
/// is filtered out here rather than sent down to be refused, so the wallet can
/// say *which* holding it cannot spend instead of failing with a builder error
/// about balances.
fn token_inputs(chain: &Chain, address: &str, source: &str) -> Result<Vec<Utxo>, ConvertError> {
    use verus_sdk::decode::{decode_output_script, OutputKind};
    use verus_sdk::network::ChainReader;

    let chain_currency = chain.chain_info()?.chain_id;
    if source == chain_currency {
        return Ok(Vec::new());
    }

    let wanted = currency_id(source)?;
    let funding = network::spendable(chain, address)?;

    let mut usable = Vec::new();
    let mut unusable = false;
    for found in &funding.other {
        // Anything that is not a reserve output — an identity, a commitment,
        // something this build does not decode — is not this conversion's
        // business and is left alone.
        if let Ok(OutputKind::ReserveOutput { tokens, .. }) =
            decode_output_script(&found.utxo.script_pubkey)
        {
            if tokens.len() == 1 && tokens[0].0 == wanted {
                usable.push(found.utxo.clone());
            } else if tokens.iter().any(|(id, _)| *id == wanted) {
                // Holds what is being converted and something else too.
                // Nameable, and worth naming: the wallet's balance says it has
                // the currency and the conversion is about to say it cannot
                // spend it, and those two look like a contradiction unless
                // somebody says why.
                unusable = true;
            }
        }
    }

    if usable.is_empty() && unusable {
        return Err(ConvertError::UnusableTokens(source.to_string()));
    }
    Ok(usable)
}

/// Read the signed conversion back out, and build the review from it.
///
/// Everything below that *can* come from the bytes does — see
/// [`ConvertReviewVm`]. On this screen that is not a nicety: a conversion's
/// entire meaning lives inside one CryptoCondition payload, so which currency,
/// how much, which basket it routes through and where the result lands are
/// invisible to anybody reading the transaction by eye, and a review that
/// echoed the form could not show any of the four being wrong.
pub fn review(
    ticket: u64,
    prepared: &Prepared,
    from_address: &str,
    spendable: Amount,
) -> pecu_protocol::ConvertReviewVm {
    let outputs = crate::send::decode_outputs(&prepared.unsent.hex, from_address);
    let sent = &prepared.unsent.outcome;

    let transfer = crate::send::conversion_in(&prepared.unsent.hex);

    // The transfer fee as it was actually written, not as it was asked for.
    // These are the same number unless something has gone wrong, which is the
    // entire reason for reading it back rather than printing the constant.
    let written_fee = transfer
        .as_ref()
        .map_or(TRANSFER_FEE_SATS, |found| found.fee_sats);

    // What actually leaves in the chain's own currency. A token conversion's
    // amount travels inside the payload and is not native at all, so folding it
    // in here would overstate the outlay by the whole amount — and understate
    // the balance left behind by the same.
    let native_out = transfer
        .as_ref()
        .map_or(prepared.amount.to_sat(), |found| found.native_sats)
        .saturating_add(sent.fee.to_sat());

    pecu_protocol::ConvertReviewVm {
        ticket,
        outputs,
        from: prepared.from_name.clone(),
        to: prepared.to_name.clone(),
        via: prepared.via.clone(),
        pay_display: format!(
            "{} {}",
            format::coins_u64(
                transfer
                    .as_ref()
                    .map_or(prepared.amount.to_sat(), |found| found.amount_sats)
            ),
            prepared.from_name
        ),
        estimate_display: format!(
            "{} {}",
            format::coins_u64(prepared.estimated.to_sat()),
            prepared.to_name
        ),
        minimum_display: format!(
            "{} {}",
            format::coins_u64(prepared.floor.to_sat()),
            prepared.to_name
        ),
        conversion_fee_display: format::coins_u64(written_fee),
        network_fee_display: format::coins_u64(sent.fee.to_sat()),
        total_display: format::coins_u64(native_out),
        balance_after_display: format::coins_u64(spendable.to_sat().saturating_sub(native_out)),
        from_address: from_address.to_string(),
        recipient: transfer
            .as_ref()
            .and_then(|found| found.recipient.clone())
            .unwrap_or_default(),
    }
}

/// Hand the bytes to a node. **Blocking, and not cancellable.**
///
/// Takes a [`SpendPermit`] because [`Chain::broadcaster`] does, which is the
/// whole of the spending guard — see [`crate::send::broadcast`], which this is
/// the twin of. Abandoning it mid-flight would manufacture exactly the
/// ambiguity `BroadcastUncertain` exists to report.
pub fn broadcast(
    chain: &Chain,
    permit: &SpendPermit,
    prepared: Prepared,
) -> Result<Sent, FlowError> {
    prepared.unsent.broadcast(&chain.broadcaster(permit))
}

#[cfg(test)]
#[allow(clippy::unreadable_literal)]
mod tests {
    use super::*;
    use crate::market::Pool;

    const VRSCTEST: &str = "iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq";
    const DAI: &str = "iN9vbHXexEh6GTZ45fRoJGKTQThfbgUwMh";
    const BRIDGE: &str = "iSojYsotVzXz4wh2eJriASGo6UidJDDhL2";

    fn names() -> BTreeMap<String, String> {
        [
            (VRSCTEST, "VRSCTEST"),
            (DAI, "DAI.vETH"),
            (BRIDGE, "Bridge.vETH"),
        ]
        .into_iter()
        .map(|(id, name)| (id.to_string(), name.to_string()))
        .collect()
    }

    fn holdings(coins: u64) -> BTreeMap<String, Amount> {
        [(VRSCTEST.to_string(), Amount::from_sat(coins * 100_000_000))]
            .into_iter()
            .collect()
    }

    /// Bridge.vETH's mid price for VRSCTEST in DAI, from the reserve state the
    /// scripted chain publishes: 7.09025643 / 13.19790409.
    const MID: f64 = 0.537_225_939_940_892_5;

    fn ready(pay: u64) -> Ready {
        Ready {
            from: VRSCTEST.to_string(),
            to: DAI.to_string(),
            amount: Amount::from_sat(pay * 100_000_000),
            pool: "Bridge.vETH".to_string(),
            pool_id: BRIDGE.to_string(),
            via: Some("Bridge.vETH".to_string()),
            mid: Some(MID),
        }
    }

    /// Every figure on the quote panel carries its currency, and the two are
    /// not the same one.
    ///
    /// The fee is taken from what goes in and the floor is what comes out, and
    /// the lines they sit on say neither — so a quote of bare numbers reads as
    /// one currency and understates the cost by a factor of two.
    ///
    /// The two figures inside the boxes do **not** carry it, because the box
    /// has the currency on a button beside the number.
    #[test]
    fn a_quote_says_which_currency_each_figure_is_in() {
        let quote = quote(
            &ready(250),
            &names(),
            &holdings(415),
            Amount::from_sat(13_352_458_143),
            Some(Amount::from_sat(12_500_000)),
            Some(Amount::from_sat(10_000)),
        );

        assert_eq!(quote.get, "133.5245 8143");
        assert_eq!(quote.conversion_fee, "0.1250 0000 VRSCTEST");
        assert_eq!(quote.network_fee, "0.0001 0000 VRSCTEST");
        assert_eq!(quote.from_balance, "415.0000 0000");
        assert!(quote.minimum.ends_with(" DAI.vETH"), "{}", quote.minimum);
        assert_eq!(quote.rate, "1 VRSCTEST = 0.5341 DAI.vETH");
        assert!(quote.ready);
    }

    /// The three tones, at the sizes that reach them.
    ///
    /// These are what the curve actually yields on Bridge.vETH's published
    /// reserves — 250 coins costs half a percent and three thousand costs six.
    /// The thresholds are a rule about money and live here, not in a binding.
    #[test]
    fn slippage_gets_its_tone_from_how_far_the_estimate_fell() {
        let cases = [
            (250_u64, 13_352_458_143_u64, "0.58%", "positive"),
            (1000, 52_570_112_000, "2.15%", "warning"),
            (3000, 151_364_254_684, "6.08%", "negative"),
        ];

        for (pay, out, want_slippage, want_tone) in cases {
            let quote = quote(
                &ready(pay),
                &names(),
                &holdings(20_000),
                Amount::from_sat(out),
                None,
                None,
            );
            assert_eq!(quote.slippage, want_slippage, "at {pay}");
            assert_eq!(quote.slippage_tone, want_tone, "at {pay}");
        }
    }

    /// A costly conversion is not left to the colour alone — from the warning
    /// threshold, not only from the loud one.
    #[test]
    fn a_costly_conversion_is_not_left_to_the_colour_alone() {
        let cheap = quote(
            &ready(250),
            &names(),
            &holdings(20_000),
            Amount::from_sat(13_352_458_143),
            None,
            None,
        );
        assert_eq!(cheap.note.code, "", "half a percent needs no sentence");

        let warned = quote(
            &ready(1000),
            &names(),
            &holdings(20_000),
            Amount::from_sat(52_570_112_000),
            None,
            None,
        );
        assert_eq!(warned.note.code, "convert-slippage-high");
        assert_eq!(warned.note.args, vec!["2.15%".to_string()]);

        let loud = quote(
            &ready(3000),
            &names(),
            &holdings(20_000),
            Amount::from_sat(151_364_254_684),
            None,
            None,
        );
        assert_eq!(loud.note.args, vec!["6.08%".to_string()]);

        // And it is still a quote. A conversion that costs a lot is one
        // somebody may still want; refusing it would be the wallet deciding.
        assert!(loud.ready);
    }

    /// The floor and the tone must mean the same three percent.
    #[test]
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn the_two_spellings_of_the_loud_threshold_agree() {
        assert_eq!((SLIPPAGE_LOUD * 1000.0).round() as u64, FLOOR_ROOM_PERMILLE);
    }

    /// An unknown fee is `—`, never zero. "The node did not say" and "it is
    /// free" are different claims and only one of them is ever true.
    #[test]
    fn an_unanswered_fee_is_unknown_rather_than_nothing() {
        let quote = quote(
            &ready(250),
            &names(),
            &holdings(415),
            Amount::from_sat(13_352_458_143),
            None,
            None,
        );
        assert_eq!(quote.conversion_fee, UNKNOWN);
        assert_eq!(quote.network_fee, UNKNOWN);
    }

    fn book() -> Book {
        let converter = verus_sdk::network::CurrencyConverter {
            converter_id: BRIDGE.to_string(),
            name: "Bridge.vETH".to_string(),
            height: 1_156_331,
            reserves: vec![VRSCTEST.to_string(), DAI.to_string()],
            definition: serde_json::Value::Null,
            last_notarization: serde_json::json!({
                "prelaunch": false,
                "currencystate": {
                    "currencyid": BRIDGE,
                    "supply": 14_147.66083307,
                    "reservecurrencies": [
                        { "currencyid": VRSCTEST, "priceinreserve": 13.19790409,
                          "reserves": 46_679.8676944, "weight": 0.25 },
                        { "currencyid": DAI, "priceinreserve": 7.09025643,
                          "reserves": 25_077.63581784, "weight": 0.25 },
                    ],
                },
            }),
        };
        let pools: Vec<Pool> = [converter]
            .iter()
            .filter_map(Pool::from_converter)
            .collect();
        Book::new(pools, DAI.to_string(), VRSCTEST.to_string())
    }

    fn draft(from: &str, to: &str, pay: &str) -> ConvertDraft {
        ConvertDraft {
            from: from.to_string(),
            to: to.to_string(),
            pay: pay.to_string(),
        }
    }

    /// Every refusal a node never has to be asked about.
    #[test]
    fn what_can_be_refused_offline_is_refused_offline() {
        let book = book();
        let names = names();
        let holdings = holdings(415);
        let refuse = |draft: ConvertDraft| {
            check(&draft, &book, &holdings, &names)
                .expect_err("should refuse")
                .code
        };

        assert_eq!(refuse(draft("", DAI, "1")), "convert-pick-both");
        assert_eq!(
            refuse(draft(VRSCTEST, VRSCTEST, "1")),
            "convert-same-currency"
        );
        assert_eq!(refuse(draft(VRSCTEST, DAI, "one")), "amount-unparsable");
        assert_eq!(refuse(draft(VRSCTEST, DAI, "0")), "amount-zero");
        assert_eq!(refuse(draft(VRSCTEST, DAI, "500")), "convert-above-holding");
        assert_eq!(
            refuse(draft(VRSCTEST, "iGRp1CGkuro3LtGazX8W1PRjVupPVfe8Pv", "1")),
            "convert-no-route",
        );

        // An empty amount is not a refusal. Nothing has been typed yet, and a
        // form that scolds before anybody has done anything is a form nobody
        // reads the second message on.
        assert_eq!(refuse(draft(VRSCTEST, DAI, "")).as_str(), "");
    }

    /// The floor printed on the quote is the floor that gets checked.
    ///
    /// Two sides need this number — the panel prints it and `prepare` hands it
    /// to the builder as `min_expected` — and if they ever computed it
    /// separately, somebody would be shown one figure and have a different one
    /// enforced. That is not a rounding bug; it is the wallet lying about the
    /// only commitment on the screen.
    #[test]
    fn the_floor_on_the_quote_is_the_floor_that_is_checked() {
        let estimated = Amount::from_sat(13_352_458_143);
        let quote = quote(&ready(250), &names(), &holdings(415), estimated, None, None);

        assert_eq!(
            quote.minimum,
            format!("{} DAI.vETH", format::coins_u64(floor(estimated).to_sat())),
        );
    }

    /// Three per cent under, and never above the estimate.
    #[test]
    fn the_floor_sits_below_what_the_node_expects() {
        let estimated = Amount::from_sat(100_000_000);
        assert_eq!(floor(estimated).to_sat(), 97_000_000);
        // Zero in, zero out, and no panic on the way: an estimate of nothing is
        // a real answer from a pool with nothing in it.
        assert_eq!(floor(Amount::ZERO).to_sat(), 0);
    }

    /// `via` names the basket to route through — and must not be sent when one
    /// side **is** that basket, which asks the node to go through the thing it
    /// is going to.
    #[test]
    fn a_conversion_into_the_basket_itself_has_no_route_to_name() {
        let book = book();
        let names = names();
        let holdings = holdings(415);

        let through =
            check(&draft(VRSCTEST, DAI, "1"), &book, &holdings, &names).expect("routable");
        assert_eq!(through.via.as_deref(), Some("Bridge.vETH"));

        let into =
            check(&draft(VRSCTEST, BRIDGE, "1"), &book, &holdings, &names).expect("routable");
        assert_eq!(into.via, None);
    }
}
