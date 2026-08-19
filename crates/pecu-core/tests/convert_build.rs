//! Building and signing a conversion, with nothing broadcast.
//!
//! # What this proves that a unit test on `check` cannot
//!
//! That the review someone reads is derived from the transaction that was
//! actually signed — and on this screen that is worth more than it is on Send.
//! A payment's outputs are legible: an address and a number. A conversion's
//! whole meaning sits inside one CryptoCondition payload, so which currency,
//! how much, what it routes through and where the result lands are invisible
//! to anybody reading the raw transaction. If the decode is wrong, the review
//! shows a plausible number rather than failing, and nobody finds out until
//! money has moved.
//!
//! So everything here goes through the SDK's real builder against a scripted
//! chain, and then the resulting bytes are deserialized and read back out — the
//! same path the Convert screen uses.
//!
//! **Zero broadcasts.** Asserted, not assumed: `prepare_conversion` takes a
//! `ChainReader` and no `Broadcaster`, so it is incapable of sending, and
//! `ScriptedReader::broadcasts()` confirms nothing reached the trait method.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use pecu_core::convert;
use verus_flows::testing::ScriptedReader;
use verus_sdk::convert::ConversionKind;
use verus_sdk::currency::CurrencyId;
use verus_sdk::money::Amount;
use verus_sdk::network::ConversionEstimate;
use verus_sdk::verus_keys::{Address, PrivateKey};

/// From the SDK's own fixtures, so the address is a known quantity.
const WIF: &str = "UusoQWsobQKUkezgBJa22D9G4t9Avo6k8wD5UUxmmfAEoTN8bawc";
const FROM: &str = "RQr2cUkF46n7y8WRzDkd1iV9gHusSSQuzX";

/// The chain's own currency, which is what `ScriptedReader` reports as its
/// `chain_id`. A conversion out of this one is native and needs no token
/// inputs.
const VRSCTEST: &str = "iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq";
/// A fractional basket on VRSCTEST, from the SDK's fixtures.
const SHYLOCK: &str = "iQihXUcQt8G9TSh58YoM5NRwC1nAyoazFR";
/// A reserve inside it, used only to name a third currency.
const DAI: &str = "iN9vbHXexEh6GTZ45fRoJGKTQThfbgUwMh";

const TIP: u32 = 1_000_000;
/// One VRSCTEST in, and what the node says comes out.
const PAY_SATS: u64 = 100_000_000;
const EXPECTED_OUT: u64 = 149_165_329;

fn id(address: &str) -> CurrencyId {
    CurrencyId::from_bytes(address.parse::<Address>().expect("a valid address").hash())
}

/// A conversion built and signed the way `convert::prepare` builds one, without
/// the vault: the key is the fixture's, and everything else is the same call.
fn prepared() -> (ScriptedReader, convert::Prepared) {
    let key = PrivateKey::from_wif(WIF).expect("the fixture WIF is valid");
    let reader = ScriptedReader::new(TIP)
        .with_utxo(FROM, TIP - 500, 100 * 100_000_000)
        .with_estimate(ConversionEstimate {
            estimated_out: Amount::from_sat(EXPECTED_OUT),
            fee: None,
        });

    let amount = Amount::from_sat(PAY_SATS);
    let floor = convert::floor(Amount::from_sat(EXPECTED_OUT));

    let unsent = verus_flows::prepare_conversion(
        &reader,
        &key,
        VRSCTEST,
        amount,
        ConversionKind::IntoFractional {
            fractional: id(SHYLOCK),
        },
        FROM,
        // The same fee `convert::prepare` writes — see `TRANSFER_FEE_SATS`.
        Amount::from_sat(20_010),
        Some(floor),
        &[],
    )
    .expect("the build succeeds");

    let prepared = convert::Prepared {
        unsent,
        from: VRSCTEST.to_string(),
        to: SHYLOCK.to_string(),
        from_name: "VRSCTEST".to_string(),
        to_name: "shylock".to_string(),
        amount,
        estimated: Amount::from_sat(EXPECTED_OUT),
        floor,
        via: String::new(),
    };

    (reader, prepared)
}

#[test]
fn a_conversion_is_built_signed_and_read_back_without_being_sent() {
    let (reader, prepared) = prepared();

    // The property the type system already guarantees, asserted anyway: this
    // path cannot have sent anything, because it was never given a broadcaster.
    assert!(
        reader.broadcasts().is_empty(),
        "preparing a conversion reached the network: {:?}",
        reader.broadcasts(),
    );

    let review = convert::review(7, &prepared, FROM, Amount::from_sat(100 * 100_000_000));

    assert_eq!(review.ticket, 7);
    assert_eq!(review.from_address, FROM);
    assert_eq!(review.from, "VRSCTEST");
    assert_eq!(review.to, "shylock");
}

/// The amount, the fee and the delivery address all come out of the payload.
///
/// This is the assertion the whole file is for. Each of these is written into a
/// serialized `CReserveTransfer` and read back through a separate decoder, so
/// agreeing is evidence rather than tautology — the request went in one way and
/// came out the other.
#[test]
fn the_review_reads_the_conversion_out_of_the_bytes_it_signed() {
    let (_reader, prepared) = prepared();
    let review = convert::review(1, &prepared, FROM, Amount::from_sat(100 * 100_000_000));

    assert_eq!(
        review.pay_display, "1.0000 0000 VRSCTEST",
        "the amount decoded out of the payload"
    );
    // 20 010 satoshis, as it was written — not as it was asked for.
    assert_eq!(review.conversion_fee_display, "0.0002 0010");
    assert_eq!(
        review.recipient, FROM,
        "the delivery address lives inside the transfer destination, not in the script"
    );
    assert_eq!(
        review.estimate_display, "1.4916 5329 shylock",
        "what the node expected, carried rather than decoded — it is not in the bytes"
    );
    assert_eq!(
        review.minimum_display, "1.4469 0369 shylock",
        "the floor, three per cent under the estimate"
    );
}

/// The conversion output is decoded as a conversion, not as an unreadable one.
///
/// Before `output-conversion` existed it fell through to "an output this build
/// cannot read", which on a review screen is the loudest thing the wallet can
/// say — and it would have said it about every conversion it ever built.
#[test]
fn the_conversion_output_is_named_rather_than_reported_as_unreadable() {
    let (_reader, prepared) = prepared();
    let review = convert::review(1, &prepared, FROM, Amount::from_sat(100 * 100_000_000));

    let conversion = review
        .outputs
        .iter()
        .find(|output| output.kind.code == "output-conversion")
        .expect("the conversion output must be recognised as one");

    assert_eq!(conversion.address.as_deref(), Some(FROM));
    assert!(
        !conversion.is_change,
        "the value on its way out is not change"
    );
    assert!(
        review.outputs.iter().any(|output| output.is_change),
        "the surplus comes back, and the review has to show it: {:#?}",
        review.outputs
    );
    assert!(
        review
            .outputs
            .iter()
            .all(|output| output.kind.code != "output-unreadable"
                && output.kind.code != "output-unrecognised"),
        "every output on a review must be legible: {:#?}",
        review.outputs
    );
}

/// What leaves the wallet natively, and what is left after.
///
/// A native conversion spends the amount plus both fees. Getting this from the
/// output's own value rather than from arithmetic over the form is what makes
/// it survive a builder that placed the value somewhere unexpected.
#[test]
fn the_review_says_what_actually_leaves_the_wallet() {
    let (_reader, prepared) = prepared();
    let held = Amount::from_sat(100 * 100_000_000);
    let review = convert::review(1, &prepared, FROM, held);

    // 1 coin + 20 010 sats transfer fee + the miner fee the builder charged.
    let leaving = review
        .total_display
        .replace([' ', ','], "")
        .parse::<f64>()
        .expect("a formatted amount");
    assert!(
        leaving > 1.0002 && leaving < 1.01,
        "a one-coin conversion should leave a little over one coin, not {}",
        review.total_display
    );

    let left = review
        .balance_after_display
        .replace([' ', ','], "")
        .parse::<f64>()
        .expect("a formatted amount");
    assert!(
        (left + leaving - 100.0).abs() < 0.000_001,
        "what leaves and what is left must add back up to what was held: {} + {} != 100",
        review.balance_after_display,
        review.total_display
    );
}

/// Which of the three conversions this is, decided by where the pool sits.
///
/// The three cases are not interchangeable — each sets different flags and a
/// different destination — and the only thing that distinguishes them is the
/// route the book found. Getting this wrong builds a transaction the chain
/// rejects, or worse, one it accepts as something else.
#[test]
fn the_kind_of_conversion_follows_the_pool_the_route_found() {
    // Buying the basket itself: a reserve goes into the fractional.
    assert_eq!(
        convert::kind_for(SHYLOCK, VRSCTEST, SHYLOCK).expect("a kind"),
        ConversionKind::IntoFractional {
            fractional: id(SHYLOCK)
        }
    );

    // Selling the basket: the fractional comes back out into a reserve.
    assert_eq!(
        convert::kind_for(SHYLOCK, SHYLOCK, VRSCTEST).expect("a kind"),
        ConversionKind::IntoReserve {
            reserve: id(VRSCTEST)
        }
    );

    // Neither leg is the pool, so the value passes through it.
    assert_eq!(
        convert::kind_for(SHYLOCK, VRSCTEST, DAI).expect("a kind"),
        ConversionKind::ReserveToReserve {
            via: id(SHYLOCK),
            target: id(DAI),
        }
    );
}

/// A leg that is not an address is refused rather than built from nonsense.
#[test]
fn a_currency_that_is_not_an_address_is_refused() {
    assert!(convert::kind_for(SHYLOCK, VRSCTEST, "not-a-currency").is_err());
}
