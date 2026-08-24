//! Emptying a key through the function the Send screen actually calls.
//!
//! # What this covers that `send_build.rs` cannot
//!
//! That file drives `send::resolve_send_all` and then calls
//! `verus_flows::prepare_send` itself. Everything between the two — opening the
//! key by label, deciding `has_smart_outputs` from the parsed destination,
//! reading the funding set inside the signing window, writing the resolved
//! figure onto `Prepared.amount` — is `send::prepare`, and none of it was ever
//! run. Its own comments name two of those steps as silent failures: a
//! differently computed `has_smart_outputs` moves the fee ladder by an input,
//! and an unset `amount` shows a zero beside a perfectly correct outputs list.
//! A harness that recomputes both cannot notice either.
//!
//! So this asks `send::prepare` the question, with a vault, a key and a chain
//! behind it, and reads the answer off `Prepared`.
//!
//! # Zero broadcasts, measured rather than argued
//!
//! `send::prepare` is handed a `ChainReader` and no `Broadcaster`, so it is
//! incapable of sending. That is a property of the types and it is worth
//! asserting anyway: the interesting failure is not "the flow sent something",
//! it is a future refactor handing the preparation step a broadcaster for some
//! unrelated convenience. `MockChain` counts attempts, and the count is zero.
//!
//! # Why the whole file is behind `--features mock`
//!
//! `send::prepare` takes a `pecu_chain::Chain`, and the only variant of that
//! which answers without a socket is `Chain::Mock`, which the feature gates.
//! Run it with `cargo test -p pecu-core --features mock`. That is the same
//! bargain `currency_launch.rs` and `demo_chain.rs` already made.

#![cfg(feature = "mock")]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use pecu_core::send;
use pecu_keystore::{NewKey, Vault};
use pecu_protocol::Secret;
use verus_sdk::money::{Amount, Txid, Utxo};
use verus_sdk::network::AddressUtxo;
use verus_sdk::verus_keys::{Address, AddressKind, PrivateKey};

/// From the SDK's own fixtures, so the address is a known quantity.
const WIF: &str = "UusoQWsobQKUkezgBJa22D9G4t9Avo6k8wD5UUxmmfAEoTN8bawc";

/// The label the key is filed under, and the one `prepare` is asked for.
const LABEL: &str = "funding";

const COIN: u64 = 100_000_000;

/// The recipient, DERIVED rather than written down — a hand-typed address has a
/// checksum, and getting it wrong fails the test for a reason that has nothing
/// to do with what is being tested.
fn recipient() -> String {
    PrivateKey::from_bytes(&[2u8; 32], true)
        .expect("a fixed scalar is a valid key")
        .address()
        .to_string()
}

/// A VerusID to pay, as twenty fixed bytes rather than a name.
///
/// Nothing looks it up: `send::prepare` decides the fee ladder from the address
/// **kind**, which is in the address itself, and that is exactly the decision
/// under test.
fn identity() -> String {
    Address::new(AddressKind::Identity, [7u8; 20]).to_string()
}

/// A distinct, well-formed transaction id per coin.
fn txid(index: u8) -> Txid {
    let mut bytes = [0u8; 32];
    bytes[0] = index;
    bytes[31] = 0x5a;
    Txid::from_internal(bytes)
}

struct Wallet {
    vault: Vault,
    chain: pecu_chain::Chain,
    mock: pecu_mock::MockChain,
    /// The temporary directory the vault lives in. Held so it outlives the
    /// vault rather than being dropped at the end of the setup function.
    _dir: tempfile::TempDir,
}

/// A wallet holding one key, against a chain where that key holds `coins`.
///
/// The script is written here rather than taken from `MockChain::demo` because
/// the shape of the coin set *is* the subject: the demo chain gives an address
/// a single spendable output, and a single output resolves the same way under
/// every rule anybody could write, correct or not.
fn wallet(coins: &[u64]) -> Wallet {
    let dir = tempfile::tempdir().expect("tempdir");
    let passphrase = Secret::new("correct horse battery staple".to_string());
    let vault = Vault::create(&dir.path().join("vault.json"), "test", &passphrase)
        .expect("a fresh vault is created");
    vault
        .unlock(&passphrase)
        .expect("the vault just made opens");

    let key = PrivateKey::from_wif(WIF).expect("the fixture WIF is valid");
    let address = key.address();
    let script = address
        .p2pkh_script_pubkey()
        .expect("a pay-to-public-key-hash address has a script");
    vault
        .add_key(LABEL, NewKey::FromWif { key })
        .expect("the key is added");

    let mut state = pecu_mock::MockState::default();
    let tip = state.tip;
    let held: Vec<AddressUtxo> = coins
        .iter()
        .enumerate()
        .map(|(index, satoshis)| AddressUtxo {
            utxo: Utxo {
                txid: txid(u8::try_from(index).expect("a handful of coins fit a u8")),
                vout: 0,
                satoshis: Amount::from_sat(*satoshis),
                script_pubkey: script.clone(),
            },
            address: address.to_string(),
            // Five hundred blocks old, so every one of them is well past
            // coinbase maturity and the maturity rule is not what this is
            // measuring.
            height: tip - 500,
            is_spendable: true,
        })
        .collect();
    state.utxos.insert(address.to_string(), held);

    let mock = pecu_mock::MockChain::new(state);
    Wallet {
        vault,
        chain: pecu_chain::Chain::Mock(mock.clone()),
        mock,
        _dir: dir,
    }
}

/// A draft that empties the key. No typed amount, because the form has no field
/// to type one into while this is set.
fn sweep_to(to: &str) -> pecu_protocol::SendDraft {
    pecu_protocol::SendDraft {
        from_label: LABEL.to_string(),
        to: to.to_string(),
        // Deliberately non-empty and deliberately absurd. `prepare` must ignore
        // whatever the form last held here — a stale string surviving a switch
        // of modes is the realistic way this field is non-empty — and reaching
        // for it instead of resolving would build a payment of ninety-nine
        // thousand coins out of a key holding five.
        amount: "99999".to_string(),
        from_pool: pecu_protocol::Pool::Transparent,
        send_all: true,
    }
}

/// The whole of the send-all path: resolve, build, sign, and report the figure.
///
/// Five whole coins and three worth 500 satoshis each — the shape from
/// `send_build.rs`'s regression test, because it is the one where the naive
/// `total − fee` and the right answer differ. Past the fee floor an extra input
/// costs about 1 800 satoshis, so the small coins cost more to move than they
/// are worth and are left where they are.
#[test]
fn preparing_a_send_all_resolves_the_amount_and_writes_it_onto_the_review() {
    let wallet = wallet(&[COIN, COIN, COIN, COIN, COIN, 500, 500, 500]);
    let to = recipient();

    let prepared = send::prepare(
        &wallet.chain,
        // One scripted node, so nothing corroborates it — the shape a
        // default install is in. `send_corroboration.rs` is where two
        // nodes are put against each other.
        None,
        &wallet.vault,
        LABEL,
        &sweep_to(&to),
        // Typed out as an address, not resolved from a name.
        "",
    )
    .expect("five whole coins can pay a fee");

    assert_eq!(
        wallet.mock.broadcast_attempts(),
        0,
        "preparing a payment reached the network",
    );

    assert_eq!(prepared.to, to);
    assert_eq!(prepared.route, pecu_protocol::Route::Transparent);

    // The field the production comment calls out: `review` echoes it for
    // `amount_display` rather than decoding it, so a `prepare` that left it as
    // the parsed draft would show a zero — or, with the draft above, 99 999
    // coins — beside a perfectly correct outputs list.
    let fee = prepared.signed.fee().to_sat();
    assert_eq!(
        prepared.amount.to_sat(),
        5 * COIN - fee,
        "the resolved amount did not reach `Prepared.amount`",
    );

    // And the property the resolution exists for. Change comes off the signed
    // outcome, not off the request.
    assert_eq!(
        prepared.signed.change(),
        Amount::ZERO,
        "a send-all produced change",
    );

    let bytes = hex::decode(prepared.signed.hex()).expect("the builder emits hex");
    let tx = verus_sdk::verus_wire::TxV4::deserialize(&bytes).expect("the builder emits a v4 tx");
    assert_eq!(tx.outputs.len(), 1, "{:#?}", tx.outputs);
    assert_eq!(
        tx.inputs.len(),
        5,
        "a coin worth less than an input costs was spent anyway",
    );
}

/// `has_smart_outputs`, decided inside `prepare` rather than by the test.
///
/// Paying a VerusID sizes every output at 200 bytes instead of 34, which crosses
/// the fee floor an input earlier — so the same coins resolve to a different
/// input set. Getting it wrong is silent: it shifts the count by one and
/// nothing on screen says so. This is the assertion that the *production*
/// function reads the destination, because the two calls below differ in
/// nothing else.
#[test]
fn preparing_a_send_all_prices_a_verusid_recipient_as_a_smart_output() {
    const COINS: [u64; 4] = [COIN, COIN, COIN, 1_000];

    let plain = wallet(&COINS);
    let prepared = send::prepare(
        &plain.chain,
        None,
        &plain.vault,
        LABEL,
        &sweep_to(&recipient()),
        "",
    )
    .expect("three whole coins can pay a fee");
    assert_eq!(
        prepared.amount.to_sat() + prepared.signed.fee().to_sat(),
        3 * COIN + 1_000,
        "with plain outputs the fourth coin still sits under the floor and is worth taking",
    );

    let smart = wallet(&COINS);
    let prepared = send::prepare(
        &smart.chain,
        None,
        &smart.vault,
        LABEL,
        &sweep_to(&identity()),
        "",
    )
    .expect("three whole coins can pay a fee");
    assert_eq!(
        prepared.amount.to_sat() + prepared.signed.fee().to_sat(),
        3 * COIN,
        "the fee ladder for an identity recipient was not used",
    );
    assert_eq!(prepared.signed.change(), Amount::ZERO);
    assert_eq!(smart.mock.broadcast_attempts(), 0);
}

/// A key that cannot pay a fee is refused, by name, with nothing signed.
///
/// The same refusal the form shows offline. It reaches here when the coins moved
/// between the keystroke and the build — and it has to survive the trip through
/// `with_key`, which is where a `?` on the wrong error type would turn a
/// sentence somebody can act on into a generic build failure.
#[test]
fn preparing_a_send_all_from_a_key_that_cannot_pay_a_fee_is_refused() {
    let wallet = wallet(&[5_000]);

    // `Prepared` holds signed bytes and deliberately does not derive `Debug`,
    // so the success arm is named rather than unwrapped into a message.
    let outcome = send::prepare(
        &wallet.chain,
        None,
        &wallet.vault,
        LABEL,
        &sweep_to(&recipient()),
        "",
    );
    match outcome {
        Err(send::SendError::NotEnoughForFee) => {}
        Err(other) => panic!("the wrong refusal: {other:?}"),
        Ok(_) => panic!("5 000 satoshis paid a 10 000 satoshi fee"),
    }
    assert_eq!(wallet.mock.broadcast_attempts(), 0);
}
