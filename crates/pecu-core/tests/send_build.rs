//! Building and signing a payment, with nothing broadcast.
//!
//! # What this proves that a unit test on `validate` cannot
//!
//! That the review someone reads is derived from the transaction that was
//! actually signed. Everything here goes through the SDK's real builder against
//! a scripted chain, and then the resulting bytes are deserialized and read back
//! out — the same path the Send screen uses.
//!
//! **Zero broadcasts.** Asserted, not assumed: `prepare_send` takes a
//! `ChainReader` and no `Broadcaster`, so it is incapable of sending, and
//! `ScriptedReader::broadcasts()` confirms nothing reached the trait method.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use pecu_core::send;
use verus_flows::testing::ScriptedReader;
use verus_sdk::money::Amount;
use verus_sdk::verus_keys::PrivateKey;

/// From the SDK's own fixtures, so the address is a known quantity.
const WIF: &str = "UusoQWsobQKUkezgBJa22D9G4t9Avo6k8wD5UUxmmfAEoTN8bawc";
const FROM: &str = "RQr2cUkF46n7y8WRzDkd1iV9gHusSSQuzX";

/// The recipient, DERIVED rather than written down.
///
/// A hand-typed address has a checksum, and getting it wrong fails the test for
/// a reason that has nothing to do with what is being tested. Deriving one from
/// a fixed scalar gives a valid address that is stable across runs.
fn recipient() -> String {
    PrivateKey::from_bytes(&[2u8; 32], true)
        .expect("a fixed scalar is a valid key")
        .address()
        .to_string()
}

const TIP: u32 = 1_000_000;

#[test]
fn a_payment_is_built_signed_and_read_back_without_being_sent() {
    let key = PrivateKey::from_wif(WIF).expect("the fixture WIF is valid");
    // One mature output of 100 coins, well past coinbase maturity.
    let reader = ScriptedReader::new(TIP).with_utxo(FROM, TIP - 500, 100 * 100_000_000);

    let to = recipient();
    let amount = Amount::from_sat(50 * 100_000_000);
    let unsent = verus_flows::prepare_send(&reader, &key, &to, amount).expect("the build succeeds");

    // The property the type system already guarantees, asserted anyway: this
    // path cannot have sent anything, because it was never given a broadcaster.
    assert!(
        reader.broadcasts().is_empty(),
        "preparing a payment reached the network: {:?}",
        reader.broadcasts(),
    );

    let prepared = send::Prepared {
        to: to.clone(),
        amount,
        // Wrapped in the route enum: signing is where the four routes stop
        // differing, and everything downstream of it is shared.
        signed: pecu_core::send::Signed::Transparent(unsent),
        route: pecu_protocol::Route::Transparent,
        // Typed out as an address, not resolved from a name.
        name: String::new(),
    };

    let review = send::review(
        7,
        &prepared,
        FROM,
        Amount::from_sat(100 * 100_000_000),
        false,
    );

    assert_eq!(review.ticket, 7);
    assert_eq!(review.from_address, FROM);

    // Two outputs: the recipient, and the change coming back. Both decoded from
    // the signed bytes — nothing here was echoed from the request.
    assert_eq!(review.outputs.len(), 2, "{:#?}", review.outputs);

    let paid = review
        .outputs
        .iter()
        .find(|output| output.address.as_deref() == Some(to.as_str()))
        .expect("the recipient is among the outputs");
    assert!(!paid.is_change);
    assert_eq!(paid.amount_display, "50.0000 0000");
    assert_eq!(paid.kind.code, "output-payment");

    let change = review
        .outputs
        .iter()
        .find(|output| output.is_change)
        .expect("change comes back to us");
    assert_eq!(change.address.as_deref(), Some(FROM));

    // The arithmetic the review exists to make visible: what leaves is the
    // payment plus the fee, and the change is not part of it.
    assert_eq!(review.amount_display, "50.0000 0000");
    let fee: u64 = strip(&review.fee_display);
    let total: u64 = strip(&review.total_display);
    assert_eq!(total, 50 * 100_000_000 + fee, "total is not amount + fee");
    assert!(fee > 0, "a payment with no fee is not a payment");

    // Change plus payment plus fee accounts for the whole input.
    let change_sats: u64 = strip(&review.change_display);
    assert_eq!(
        change_sats + 50 * 100_000_000 + fee,
        100 * 100_000_000,
        "the coins do not add up",
    );
}

/// Paying an amount the wallet does not have must fail at build time, before
/// anything is signed.
#[test]
fn a_payment_beyond_the_balance_is_refused_before_signing() {
    let key = PrivateKey::from_wif(WIF).expect("the fixture WIF is valid");
    let reader = ScriptedReader::new(TIP).with_utxo(FROM, TIP - 500, 100_000_000);

    let error =
        verus_flows::prepare_send(&reader, &key, &recipient(), Amount::from_sat(500_000_000))
            .expect_err("50 coins cannot come out of one");

    assert!(
        matches!(error, verus_flows::FlowError::InsufficientFunds { .. }),
        "{error:?}",
    );
    assert!(reader.broadcasts().is_empty());
}

/// An immature coinbase is owned and not spendable, and the difference is the
/// whole reason the dashboard shows two numbers.
#[test]
fn an_immature_coinbase_cannot_be_spent() {
    let key = PrivateKey::from_wif(WIF).expect("the fixture WIF is valid");
    // Mined ten blocks ago: 90 short of the 100-confirmation maturity.
    let reader = ScriptedReader::new(TIP).with_coinbase_at(TIP - 10);

    let error = verus_flows::prepare_send(&reader, &key, &recipient(), Amount::from_sat(100_000))
        .expect_err("an immature coinbase is not spendable");

    assert!(
        matches!(error, verus_flows::FlowError::InsufficientFunds { .. }),
        "{error:?}",
    );
}

/// `"12 482.4200 0000"` → `1248242000000`. The display format is grouped and
/// padded; the arithmetic is on satoshis.
fn strip(display: &str) -> u64 {
    display
        .chars()
        .filter(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .expect("a display amount is digits and separators")
}

/// Prints the derived recipient, so the UI fixture can use a real address
/// rather than a hand-typed one with a broken checksum.
///
/// ```sh
/// cargo test -p pecu-core --test send_build -- --ignored --nocapture recipient
/// ```
#[ignore = "prints a fixture value"]
#[test]
fn the_recipient_address() {
    println!("{}", recipient());
}
