//! A `t→z` built, proven and signed — against a scripted chain, with the real
//! prover.
//!
//! # Why this is `#[ignore]`
//!
//! It needs the ~50 MB Sapling parameters, which are on a machine that runs a
//! node and are not in this repository. It is also slow: seconds to load the
//! circuits and tens of seconds to prove. Both make it a test somebody runs
//! deliberately rather than one that runs on every push.
//!
//!   cargo test -p pecu-core --test shield_build -- --ignored --nocapture
//!
//! # What it proves, and what it cannot
//!
//! It proves the composition: funding is selected, the fee follows the daemon's
//! output-counted rule, the bundle is proven, the binding signature is applied,
//! the transparent inputs are signed afterwards, and the whole thing serializes
//! to something with a txid. Every one of those is a place a shield fails
//! silently.
//!
//! It cannot prove the chain will accept it. Only a broadcast does that, and
//! `PROVEN.md` records the SDK's own — `35eccaca…` at block 1166308.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;
use std::time::Instant;

use pecu_core::{params, shield};
use verus_flows::testing::ScriptedReader;
use verus_sdk::verus_keys::PrivateKey;

/// The SDK's fixture key, and the address it controls.
const WIF: &str = "UusoQWsobQKUkezgBJa22D9G4t9Avo6k8wD5UUxmmfAEoTN8bawc";
const FROM: &str = "RQr2cUkF46n7y8WRzDkd1iV9gHusSSQuzX";

/// The SDK's own `zaddr` test vector — a real Sapling address, so the decode
/// and the note encryption are exercised on something well-formed.
const TO: &str = "zs18pytujp8qu73a3fu6g9chl7mfumrr0htyqsh60r3ed4capagqwm8tx2l8f9c5g7w87q4566uph3";

const TIP: u32 = 1_200_000;

fn parameters() -> verus_sdk::verus_sapling::params::SaplingParams {
    let home = PathBuf::from(std::env::var("HOME").expect("HOME"));
    let located = params::find(&home.join(".pecu-does-not-exist"))
        .expect("the Sapling parameters must be on this machine to run this test");
    params::load(&located).expect("the parameters load")
}

/// One shield, all the way to signed bytes.
#[test]
#[ignore = "needs the Sapling parameters and tens of seconds of proving"]
fn a_shield_is_planned_proven_and_signed_without_being_sent() {
    // 100 VRSCTEST in one output.
    let reader = ScriptedReader::new(TIP).with_utxo(FROM, TIP - 500, 100 * 100_000_000);

    let planned = shield::plan(&reader, FROM, TO, "10").expect("plan");

    println!("amount  {}", planned.amount.to_coins_string());
    println!("fee     {}", planned.fee.to_coins_string());
    println!("change  {}", planned.change.to_coins_string());

    assert_eq!(planned.amount.to_sat(), 10 * 100_000_000);
    // The daemon counts outputs, not bytes: two shielded (a bundle is padded to
    // two whatever it carries) plus the change output.
    assert_eq!(planned.fee.to_sat(), 10_000, "not the daemon's own floor");
    // 100 in, 10 shielded, 0.0001 to the miner.
    assert_eq!(
        planned.change.to_sat(),
        100 * 100_000_000 - 10 * 100_000_000 - 10_000
    );

    // Nothing has been sent, and nothing could have been: `plan` was given a
    // reader, which has no broadcaster on it.
    assert!(
        reader.broadcasts().is_empty(),
        "planning a shield reached the network: {:?}",
        reader.broadcasts(),
    );

    let params = parameters();
    let key = PrivateKey::from_wif(WIF).expect("the fixture WIF is valid");

    let started = Instant::now();
    let prepared = shield::prepare_with_key(&key, &params, &planned).expect("prove and sign");
    println!("proved and signed in {:.1?}", started.elapsed());
    println!("txid    {}", prepared.txid);
    println!("bytes   {}", prepared.hex.len() / 2);

    // A txid is 32 bytes, hex.
    assert_eq!(prepared.txid.len(), 64);
    assert!(prepared.txid.chars().all(|c| c.is_ascii_hexdigit()));

    // A proven shield is large: two shielded outputs at ~948 bytes each, plus
    // the proofs. Anything near a bare transparent payment means the bundle
    // never got built.
    let bytes = prepared.hex.len() / 2;
    assert!(
        bytes > 2_000,
        "only {bytes} bytes — that is not a proven Sapling bundle",
    );

    // Still nothing sent.
    assert!(reader.broadcasts().is_empty());
}

/// The fee floor does not depend on how many inputs it took to reach it.
///
/// A shield paid from twenty small outputs is a much larger transaction than
/// one paid from a single big one, and the daemon charges the same for both —
/// it counts outputs. A wallet that priced this by size would overpay on the
/// first and, worse, could underpay on the second.
#[test]
fn the_fee_is_the_same_however_many_inputs_it_takes() {
    let one = ScriptedReader::new(TIP).with_utxo(FROM, TIP - 500, 100 * 100_000_000);
    let mut many = ScriptedReader::new(TIP);
    for _ in 0..20 {
        many = many.with_utxo(FROM, TIP - 500, 5 * 100_000_000);
    }

    let from_one = shield::plan(&one, FROM, TO, "10").expect("plan");
    let from_many = shield::plan(&many, FROM, TO, "10").expect("plan");

    assert_eq!(from_one.fee, from_many.fee);
}

/// Refusing to shield more than the key holds must say so with the numbers.
#[test]
fn shielding_more_than_is_there_is_refused_with_the_figures() {
    let reader = ScriptedReader::new(TIP).with_utxo(FROM, TIP - 500, 100_000_000);

    match shield::plan(&reader, FROM, TO, "10") {
        Err(shield::ShieldError::NotEnough {
            held,
            wanted,
            needed,
        }) => {
            // `to_coins_string` trims trailing zeros, which is what somebody
            // reading a refusal wants: "1", not "1.00000000".
            assert_eq!(held, "1");
            assert_eq!(wanted, "10");
            // The amount plus the fee, not the amount alone — the number that
            // actually has to be there.
            assert_eq!(needed, "10.0001");
        }
        other => panic!("wrong outcome for an unaffordable shield: {other:?}"),
    }
}

/// A transparent address is not a shielded one, and the refusal must say which
/// thing is wrong rather than reporting a bad amount later.
#[test]
fn a_transparent_destination_is_refused() {
    let reader = ScriptedReader::new(TIP).with_utxo(FROM, TIP - 500, 100 * 100_000_000);

    assert!(matches!(
        shield::plan(&reader, FROM, FROM, "1"),
        Err(shield::ShieldError::BadAddress),
    ));
}
