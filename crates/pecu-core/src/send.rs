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

use pecu_chain::{Chain, SpendPermit};
use pecu_keystore::{Vault, VaultError};
use pecu_protocol::{NoteVm, ReviewOutputVm, SendDraft, SendReviewVm};
use verus_sdk::money::Amount;
use verus_sdk::network::{self, FlowError, Sent, Unsent};
use verus_sdk::verus_keys::{Address, AddressKind};

use crate::portfolio::coins;

#[derive(Debug, thiserror::Error)]
pub enum SendError {
    #[error("that is not an address this wallet can pay")]
    BadAddress,
    #[error("that is not an amount")]
    BadAmount,
    #[error("send nothing and nothing happens")]
    NothingToSend,
    #[error(transparent)]
    Vault(#[from] VaultError),
    #[error(transparent)]
    Flow(#[from] FlowError),
}

/// A signed payment that has not been sent, and what it is for.
///
/// **Never leaves the core.** The UI gets a [`SendReviewVm`] and a ticket
/// number; the bytes stay here. A UI that held the signed hex would be a UI
/// that could be made to send it.
pub struct Prepared {
    pub unsent: Unsent<Sent>,
    pub to: String,
    pub amount: Amount,
    /// The VerusID name `to` was resolved from, when it was typed as a name.
    /// Empty otherwise. Carried so the review can show the question as well as
    /// the answer.
    pub name: String,
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
    let (to_valid, to_note) = match draft.to.trim() {
        "" => (false, NoteVm::none()),
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

    let (amount_valid, amount_note) = match draft.amount.trim() {
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
    };

    pecu_protocol::DraftValidationVm {
        to_valid,
        to_note,
        amount_valid,
        amount_note,
        // Filled in by the caller, which is the side that knows what this
        // wallet has called the address.
        to_label: String::new(),
        ready: to_valid && amount_valid,
    }
}

// ── Building ────────────────────────────────────────────────────────────────

/// Build and sign, without sending. **Blocking.**
///
/// The private key exists for the duration of `with_key` and no longer — the
/// build, the signature and the drop all happen inside that closure. There is
/// no accessor anywhere in this workspace that returns one.
pub fn prepare(
    chain: &Chain,
    vault: &Vault,
    label: &str,
    draft: &SendDraft,
    name: &str,
) -> Result<Prepared, SendError> {
    let to = draft.to.trim();
    to.parse::<Address>().map_err(|_| SendError::BadAddress)?;

    let amount = Amount::from_coins_str(draft.amount.trim()).map_err(|_| SendError::BadAmount)?;
    if amount.is_zero() {
        return Err(SendError::NothingToSend);
    }

    let unsent = vault.with_key(label, |key| network::prepare_send(chain, key, to, amount))??;

    Ok(Prepared {
        unsent,
        to: to.to_string(),
        amount,
        name: name.to_string(),
    })
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
    let outputs = decode_outputs(&prepared.unsent.hex, from);
    let sent = &prepared.unsent.outcome;

    // Total leaving the wallet: what the recipient gets plus the fee. Change is
    // not part of it — it comes back — which is exactly the arithmetic a review
    // exists to make visible.
    let total = prepared
        .amount
        .checked_add(sent.fee)
        .unwrap_or(prepared.amount);

    SendReviewVm {
        ticket,
        outputs,
        amount_display: coins(prepared.amount),
        fee_display: coins(sent.fee),
        total_display: coins(total),
        change_display: coins(sent.change),
        balance_after_display: coins(spendable.checked_sub(total).unwrap_or(Amount::ZERO)),
        from_address: from.to_string(),
        first_time_recipient: !known_recipient,
        recipient_name: prepared.name.clone(),
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
    prepared.unsent.broadcast(&chain.broadcaster(permit))
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

    fn draft(to: &str, amount: &str) -> SendDraft {
        SendDraft {
            from_label: "main".to_string(),
            to: to.to_string(),
            amount: amount.to_string(),
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

    /// Garbage in must not panic: this runs on every keystroke.
    #[test]
    fn a_transaction_that_cannot_be_decoded_yields_no_outputs() {
        assert!(decode_outputs("not hex", ADDRESS).is_empty());
        assert!(decode_outputs("deadbeef", ADDRESS).is_empty());
        assert!(decode_outputs("", ADDRESS).is_empty());
    }
}
