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
        // A transparent payment publishes no nullifiers.
        spends: Vec::new(),
        // Typed out as an address, not resolved from a name.
        name: String::new(),
        // Built here from one scripted reader, so nothing corroborated it.
        // What this file is about is the arithmetic of a send-all, and the
        // corroborator has its own tests.
        corroborated_by: String::new(),
        withheld: send::Withheld::default(),
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

// ── Emptying a key ──────────────────────────────────────────────────────────
//
// The crux of "send everything". The amount is not the balance minus a fee:
// the fee depends on how many inputs the transaction has, and how many inputs
// it has depends on the amount. `send::resolve_send_all` breaks that by
// choosing the input set first — see its doc. These tests drive the SDK's real
// selector against a scripted chain and check the one property that says the
// rule is right: **the transaction has one output and no change.**
//
// What they do NOT cover is `send::prepare` itself. The harness below resolves
// and builds in two calls of its own, so a `has_smart_outputs` computed
// differently in production, or a `Prepared.amount` left unset, would pass
// every assertion here. That is `tests/send_prepare.rs`, which goes through the
// shipping function with a vault and a key behind it — and which needs
// `--features mock`, because the only `Chain` that answers without a socket is
// the scripted one.

const COIN: u64 = 100_000_000;

/// What a send-all against these coins actually builds.
///
/// Resolves, then builds through the same SDK entry point `send::prepare` uses,
/// then deserializes the signed bytes. Nothing here is echoed back from the
/// request.
struct SweptKey {
    amount: Amount,
    fee: u64,
    change: u64,
    tx: verus_sdk::verus_wire::TxV4,
}

fn sweep(coins: &[u64], to: &str) -> Result<SweptKey, pecu_core::send::SendError> {
    let key = PrivateKey::from_wif(WIF).expect("the fixture WIF is valid");
    let mut reader = ScriptedReader::new(TIP);
    for value in coins {
        reader = reader.with_utxo(FROM, TIP - 500, *value);
    }

    let funding = verus_flows::spendable(&reader, FROM).expect("the scripted chain answers");
    // The same rule `prepare` applies, restated here because this harness does
    // not call it: a VerusID recipient makes every output 200 bytes rather than
    // 34, and the fee ladder moves with it. That the *production* function
    // applies it too is asserted in `tests/send_prepare.rs`, not here.
    let smart = to
        .parse::<verus_sdk::verus_keys::Address>()
        .is_ok_and(|address| address.kind() == verus_sdk::verus_keys::AddressKind::Identity);

    let amount = send::resolve_send_all(&funding, smart)?;
    let unsent = verus_flows::prepare_send(&reader, &key, to, amount).expect("the build succeeds");

    assert!(
        reader.broadcasts().is_empty(),
        "emptying a key reached the network: {:?}",
        reader.broadcasts(),
    );

    let bytes = hex::decode(&unsent.hex).expect("the builder emits hex");
    Ok(SweptKey {
        amount,
        fee: unsent.outcome.fee.to_sat(),
        change: unsent.outcome.change.to_sat(),
        tx: verus_sdk::verus_wire::TxV4::deserialize(&bytes).expect("the builder emits a v4 tx"),
    })
}

/// The whole property, in one assertion: everything selected left the key.
fn leaves_nothing_behind(swept: &SweptKey, spent: u64) {
    assert_eq!(swept.change, 0, "a send-all produced change");
    assert_eq!(
        swept.tx.outputs.len(),
        1,
        "a send-all produced more than the payment: {:#?}",
        swept.tx.outputs,
    );
    assert_eq!(
        swept.amount.to_sat() + swept.fee,
        spent,
        "the selected coins are not fully accounted for",
    );
}

#[test]
fn sending_everything_from_one_coin_leaves_the_key_empty() {
    let to = recipient();
    let swept = sweep(&[COIN], &to).expect("one whole coin covers a fee");

    assert_eq!(swept.tx.inputs.len(), 1);
    leaves_nothing_behind(&swept, COIN);
    assert_eq!(swept.amount.to_sat(), COIN - swept.fee);
}

/// Under five inputs the fee sits on its floor, so every coin is worth taking.
#[test]
fn sending_everything_from_four_coins_pays_the_floor_fee_and_leaves_no_change() {
    let to = recipient();
    let swept = sweep(&[COIN, COIN, COIN, COIN], &to).expect("four coins cover a fee");

    assert_eq!(swept.tx.inputs.len(), 4, "a coin was left out at the floor");
    assert_eq!(swept.fee, 10_000, "four inputs still fit under the floor");
    leaves_nothing_behind(&swept, 4 * COIN);
}

/// Past five inputs the fee is priced by size, so it grows with each coin —
/// and the amount has to follow it down. This is the case the naive
/// `total − fee` happens to survive, which is why it is not the only test here.
#[test]
fn sending_everything_from_six_coins_pays_by_size_and_still_leaves_no_change() {
    let to = recipient();
    let swept = sweep(&[COIN; 6], &to).expect("six coins cover a fee");

    assert_eq!(swept.tx.inputs.len(), 6);
    assert!(
        swept.fee > 10_000,
        "six inputs should have crossed the fee floor, paid {}",
        swept.fee,
    );
    leaves_nothing_behind(&swept, 6 * COIN);
}

/// **The regression test for the crux.**
///
/// Five whole coins and three worth 500 satoshis each. Past the floor an extra
/// input costs about 1 800 satoshis, so taking a 500-satoshi coin makes the
/// recipient *worse off* — and the selector knows it: handed an amount priced
/// for all eight it simply stops at five and hands the difference back as
/// change. `total − estimate_fee(8)` builds a transaction that pays change to a
/// key somebody has just been told is empty.
///
/// So the right answer leaves those coins where they are, takes five inputs,
/// and produces no change at all. What is left behind is 1 500 satoshis that
/// cost more to move than they are worth — which the send form warns about in
/// prose and the review shows as "Left afterwards", but which nothing names as
/// *this* payment's leftovers. `docs/LATER.md` §13 carries what that would
/// cost. It is a sentence the interface owes, not arithmetic to change.
#[test]
fn sending_everything_leaves_behind_a_coin_that_costs_more_to_spend_than_it_is_worth() {
    let to = recipient();
    // The five whole coins are seeded first, so they are `vout` 0–4 and the
    // sub-marginal ones are 5, 6 and 7.
    let swept = sweep(&[COIN, COIN, COIN, COIN, COIN, 500, 500, 500], &to)
        .expect("five whole coins cover a fee");

    assert_eq!(
        swept.tx.inputs.len(),
        5,
        "a coin worth less than an input costs was spent anyway",
    );
    assert!(
        !swept.tx.inputs.iter().any(|input| input.vout >= 5),
        "one of the 500-satoshi coins was taken",
    );
    leaves_nothing_behind(&swept, 5 * COIN);
}

/// Paying a VerusID sizes every output at 200 bytes instead of 34, which puts
/// the transaction over the fee floor an input earlier — so the same coins
/// resolve to a different input set. Getting `has_smart_outputs` wrong here is
/// silent: it shifts the count by one and nothing on screen says so.
#[test]
fn sending_everything_to_a_verusid_prices_the_output_as_a_smart_output() {
    let coins = [COIN, COIN, COIN, 1_000];

    // Plain outputs: four inputs still sit on the floor, so the small coin is
    // free to take and is taken.
    let plain = sweep(&coins, &recipient()).expect("three whole coins cover a fee");
    assert_eq!(plain.tx.inputs.len(), 4);
    assert_eq!(plain.fee, 10_000);
    leaves_nothing_behind(&plain, 3 * COIN + 1_000);

    // The same coins to an identity: the fourth input costs 1 800 satoshis and
    // the coin is worth 1 000, so it stays where it is.
    let identity = verus_sdk::verus_keys::Address::new(
        verus_sdk::verus_keys::AddressKind::Identity,
        [7u8; 20],
    )
    .to_string();
    let smart = sweep(&coins, &identity).expect("three whole coins cover a fee");
    assert_eq!(
        smart.tx.inputs.len(),
        3,
        "the fee ladder for an identity recipient was not used",
    );
    leaves_nothing_behind(&smart, 3 * COIN);
}

/// Nothing is signed, and the refusal is a sentence rather than an underflow.
#[test]
fn sending_everything_from_a_balance_below_the_fee_is_refused_before_signing() {
    let reader = ScriptedReader::new(TIP).with_utxo(FROM, TIP - 500, 5_000);
    let funding = verus_flows::spendable(&reader, FROM).expect("the scripted chain answers");

    let error = send::resolve_send_all(&funding, false)
        .expect_err("5 000 satoshis cannot pay a 10 000 satoshi fee");
    assert!(
        matches!(error, pecu_core::send::SendError::NotEnoughForFee),
        "{error:?}",
    );
    assert!(reader.broadcasts().is_empty());
}

/// An empty key is the same refusal, and must not divide by anything.
#[test]
fn sending_everything_from_a_key_with_no_coins_is_refused() {
    let reader = ScriptedReader::new(TIP);
    let funding = verus_flows::spendable(&reader, FROM).expect("the scripted chain answers");

    assert!(matches!(
        send::resolve_send_all(&funding, false),
        Err(pecu_core::send::SendError::NotEnoughForFee),
    ));
}

/// The assertion that would have caught `total − fee`, over shapes nobody
/// thought to write down.
///
/// Deliberately small and deliberately deterministic — a fixed generator rather
/// than a property-testing dependency, because the interesting space here is
/// tiny: what matters is coins either side of the marginal input cost, mixed
/// with coins far above it, at counts either side of the fee floor.
#[test]
fn sending_everything_never_leaves_a_change_output_however_the_coins_are_shaped() {
    // A coin worth about what an input costs is the whole difficulty, so the
    // ladder is dense around 1 800 satoshis.
    const VALUES: [u64; 8] = [546, 1_000, 1_799, 1_800, 1_801, 5_000, 250_000, COIN];

    let to = recipient();
    let identity = verus_sdk::verus_keys::Address::new(
        verus_sdk::verus_keys::AddressKind::Identity,
        [9u8; 20],
    )
    .to_string();

    // A linear congruential generator with its seed written down here, so
    // every run tests the same four hundred shapes and a failure reproduces by
    // running the test again. Deliberately not drawn from the clock: a case
    // that fails on one machine once and never again is a case nobody fixes.
    let mut seed: u64 = 0x5eed_1234_9abc_def1;
    let mut next = move || {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        seed >> 33
    };

    let mut swept_count = 0_u32;
    for case in 0..400_u32 {
        let mut pick = || usize::try_from(next()).expect("a 31-bit draw fits a usize");
        let count = 1 + pick() % 9;
        let coins: Vec<u64> = (0..count).map(|_| VALUES[pick() % VALUES.len()]).collect();
        let smart = case % 3 == 0;
        let to = if smart { &identity } else { &to };

        match sweep(&coins, to) {
            // A set whose whole value cannot cover a fee. Refused, not built —
            // which is the other half of the property.
            //
            // Checked against the fee for spending *all* of them, which is the
            // largest this set could ever be charged: if the total were above
            // it, taking every coin would have left something over and the
            // refusal would be wrong. Asked of the SDK rather than written
            // down, so this cannot drift from the ladder being tested.
            Err(pecu_core::send::SendError::NotEnoughForFee) => {
                let inputs = u64::try_from(coins.len()).expect("nine coins fit a u64");
                let dearest = verus_sdk::verus_tx::estimate_fee(
                    inputs,
                    2,
                    verus_sdk::money::DEFAULT_FEE_PER_KB,
                    smart,
                )
                .expect("a fee for nine inputs cannot overflow");
                assert!(
                    coins.iter().sum::<u64>() <= dearest,
                    "refused a set that could have paid: {coins:?}",
                );
            }
            Err(other) => panic!("unexpected refusal for {coins:?}: {other:?}"),
            Ok(swept) => {
                swept_count += 1;
                assert_eq!(swept.change, 0, "change left over for {coins:?}");
                assert_eq!(
                    swept.tx.outputs.len(),
                    1,
                    "more than the payment for {coins:?}: {:#?}",
                    swept.tx.outputs,
                );
                // And no coin was taken that was not worth taking: the inputs
                // are a prefix of the coins sorted descending.
                let mut sorted = coins.clone();
                sorted.sort_unstable_by(|a, b| b.cmp(a));
                let taken: u64 = sorted.iter().take(swept.tx.inputs.len()).sum();
                assert_eq!(
                    swept.amount.to_sat() + swept.fee,
                    taken,
                    "the inputs are not the largest coins for {coins:?}",
                );
            }
        }
    }

    // A floor on the generator, not on the rule: about three quarters of these
    // shapes build and the rest are legitimately too poor to pay a fee. It is
    // here so that a change which quietly turned every case into a refusal
    // could not pass this test by asserting nothing.
    assert!(
        swept_count > 250,
        "the generator produced too few buildable cases to prove anything: {swept_count}",
    );
}

/// The resolved figure has to be written onto `Prepared` by hand.
///
/// The review decodes the outputs, the fee and the change from the signed
/// bytes — but `amount_display` is `coins(prepared.amount)`, a plain field. A
/// send-all that left it as the parsed draft would show a zero beside a
/// perfectly correct outputs list.
#[test]
fn the_review_of_a_send_all_shows_the_resolved_amount_and_no_change() {
    let key = PrivateKey::from_wif(WIF).expect("the fixture WIF is valid");
    let reader = ScriptedReader::new(TIP)
        .with_utxo(FROM, TIP - 500, COIN)
        .with_utxo(FROM, TIP - 500, COIN);

    let funding = verus_flows::spendable(&reader, FROM).expect("the scripted chain answers");
    let amount = send::resolve_send_all(&funding, false).expect("two coins cover a fee");

    let to = recipient();
    let unsent = verus_flows::prepare_send(&reader, &key, &to, amount).expect("the build succeeds");

    let prepared = send::Prepared {
        to: to.clone(),
        amount,
        signed: pecu_core::send::Signed::Transparent(unsent),
        route: pecu_protocol::Route::Transparent,
        spends: Vec::new(),
        name: String::new(),
        corroborated_by: String::new(),
        withheld: send::Withheld::default(),
    };
    let review = send::review(1, &prepared, FROM, Amount::from_sat(2 * COIN), false);

    assert_eq!(strip(&review.amount_display), amount.to_sat());
    assert_eq!(review.change_display, "0.0000 0000");
    assert_eq!(review.outputs.len(), 1, "{:#?}", review.outputs);
    assert!(!review.outputs[0].is_change);
    // Everything the key could spend leaves it: the payment plus the fee is the
    // whole balance, and "left afterwards" is nothing.
    assert_eq!(strip(&review.total_display), 2 * COIN);
    assert_eq!(review.balance_after_display, "0.0000 0000");
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
