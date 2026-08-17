//! What this wallet builds is something the SDK will serialise.
//!
//! # What this covers, and what it deliberately does not
//!
//! The *choreography* of a launch — reading the identity from the chain's own
//! bytes, refusing one that already defines a currency, funding the fee — is
//! covered by twenty-two tests in `verus-flows/tests/launch_flow.rs`. Standing
//! that scaffolding up again here would be re-testing the SDK with this
//! wallet's spelling, and it needs a scripted identity, its primary script and
//! a raw transaction holding its real output bytes.
//!
//! What is *this wallet's* to get wrong is the definition it hands over:
//! `currency::definition` fills in twelve fields of a twenty-eight-field
//! consensus struct from a form. So this feeds each of the three kinds through
//! the SDK's own serialiser, which is the same code that would run on the way
//! to a signature and which refuses several shapes by name.
//!
//! A refusal here means this wallet builds definitions the chain would not take
//! — and finding that out costs nothing, where finding it out on VRSCTEST costs
//! two hundred coins and an identity that can never define another currency.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;

use pecu_core::currency;
use verus_sdk::currency::{serialize_definition, CurrencyId};
use verus_sdk::money::Amount;

const TIP: u32 = 1_000_000;

/// VRSCTEST's own currency, as the parent every definition here hangs off.
fn parent() -> CurrencyId {
    CurrencyId::from_bytes(
        "iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq"
            .parse::<verus_sdk::verus_keys::Address>()
            .expect("the chain's own i-address is valid")
            .hash(),
    )
}

/// A real VRSCTEST i-address, so the reserve and recipient parses are honest.
const RESERVE: &str = "iGRp1CGkuro3LtGazX8W1PRjVupPVfe8Pv";
const SECOND_RESERVE: &str = "i5Qcj82gvrHdHCCvTwy2yCFeMz3s3dgB6m";

fn draft(kind: &str) -> pecu_protocol::CurrencyDraft {
    pecu_protocol::CurrencyDraft {
        kind: kind.to_string(),
        identity: RESERVE.to_string(),
        new_name: String::new(),
        mintable: false,
        start_delay: "20".to_string(),
        reserves: Vec::new(),
        preallocations: Vec::new(),
    }
}

/// A reserve as the picker produces one: the i-address the definition carries,
/// and the name it was chosen by.
fn reserve(currency: &str, weight: &str) -> pecu_protocol::ReserveDraft {
    pecu_protocol::ReserveDraft {
        currency: currency.to_string(),
        name: "a reserve".to_string(),
        weight: weight.to_string(),
    }
}

fn allocation(to: &str, amount: &str) -> pecu_protocol::PreallocationDraft {
    pecu_protocol::PreallocationDraft {
        recipient: to.to_string(),
        amount: amount.to_string(),
    }
}

fn resolved() -> BTreeMap<String, [u8; 20]> {
    let mut map = BTreeMap::new();
    map.insert(RESERVE.to_string(), [3; 20]);
    map
}

/// A mintable token with no starting supply — the shape somebody launches when
/// the supply arrives later.
#[test]
fn a_mintable_token_serialises() {
    let mut token = draft("token");
    token.mintable = true;

    let built = currency::definition(&token, "demo", parent(), u64::from(TIP + 20), &resolved())
        .expect("builds");
    let bytes = serialize_definition(&built).expect("the SDK serialises what this wallet built");
    assert!(!bytes.is_empty());
}

/// A fixed-supply token, which means a preallocation — the other half of the
/// choice that cannot be changed afterwards.
#[test]
fn a_fixed_supply_token_serialises_with_its_preallocation() {
    let mut token = draft("token");
    token.preallocations = vec![allocation(RESERVE, "1000")];

    let built = currency::definition(&token, "demo", parent(), u64::from(TIP + 20), &resolved())
        .expect("builds");
    assert_eq!(built.preallocations.len(), 1);
    assert_eq!(
        built.preallocations[0].amount,
        Amount::from_sat(1000 * 100_000_000),
    );
    serialize_definition(&built).expect("the SDK serialises a preallocated token");
}

/// The one the SDK has no constructor for.
///
/// A basket is `token()` plus a bit plus two parallel vectors, and the
/// serialiser refuses a per-reserve vector whose length does not match the
/// reserve list — the failure that would otherwise attribute an amount to the
/// wrong currency, silently and permanently.
#[test]
fn a_basket_serialises_with_matched_reserve_vectors() {
    let mut basket = draft("basket");
    basket.reserves = vec![reserve(RESERVE, "40"), reserve(SECOND_RESERVE, "60")];

    let built = currency::definition(
        &basket,
        "market",
        parent(),
        u64::from(TIP + 20),
        &resolved(),
    )
    .expect("builds");

    assert_eq!(built.currencies.len(), built.weights.len());
    serialize_definition(&built).expect("the SDK serialises a two-reserve basket");
}

/// An NFT is one satoshi to one holder, and the SDK checks that itself.
#[test]
fn an_nft_serialises_as_exactly_one_unit() {
    let mut nft = draft("nft");
    nft.preallocations = vec![allocation(RESERVE, "1")];

    let built = currency::definition(&nft, "art", parent(), u64::from(TIP + 20), &resolved())
        .expect("builds");
    assert_eq!(built.preallocations[0].amount, Amount::from_sat(1));
    serialize_definition(&built).expect("the SDK serialises an NFT this wallet built");
}

/// The fee splits in half, and the review shows both halves.
///
/// One half becomes the new currency's reserve deposit output; the other is
/// burned with no output at all. Both are funded. A wallet that showed only the
/// policy figure would be naming a smaller number than the one leaving the
/// wallet.
#[test]
fn the_cost_splits_into_two_halves_that_add_back_up() {
    let fee = Amount::from_sat(200 * 100_000_000);
    let split = currency::cost(fee);

    assert_eq!(split.launch_fee, fee);
    assert_eq!(
        split.deposit.to_sat() + split.burned.to_sat(),
        fee.to_sat(),
        "the halves do not add back to the fee, so the review cannot reconcile",
    );

    // An odd satoshi has to land somewhere rather than vanishing.
    let odd = currency::cost(Amount::from_sat(7));
    assert_eq!(odd.deposit.to_sat() + odd.burned.to_sat(), 7);
}
