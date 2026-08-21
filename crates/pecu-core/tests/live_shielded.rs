//! Scanning and witnessing against the real chain.
//!
//! `#[ignore]`, because it reaches the network. Nothing else is needed —
//! `PECU_LIGHT_URL` overrides the server, and defaults to the one the chain
//! ships:
//!
//!   cargo test -p pecu-core --test live_shielded -- --ignored --nocapture
//!
//! # What this proves that the fixture tests cannot
//!
//! `shielded_scan.rs` replays committed bytes. This asks the chain. The note it
//! looks for is real and public — the SDK shielded 5 VRSCTEST to a key it
//! generated on 2026-07-29 and published the **viewing** key, so anybody can
//! find and value that note and nobody can spend it.
//!
//! Getting the same answer from a live server as from the committed blocks is
//! what says the wallet's scan is talking to the chain correctly rather than
//! merely parsing a file correctly.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use pecu_chain::{LightServer, Network};
use pecu_core::shielded::Shielded;
use pecu_keystore::ShieldedView;

/// The watch-only key for the address that received the note.
const DFVK: &str = "549a3f248605a85c02f38a2a54ee7e44384b0b8f7a875fe5e99601dd3959b0e3\
                    2dcb8b5295047e9cccb092cd4553c15b1e230daa6cc96716e7b9604008eac528\
                    5a205bfb257a272d990607e45073be515724a4bc6456d6fb5d964fcc74ee3dff\
                    90ec3f14906942bb6f572090a83bb484320a9fe310cbba8cc58ccf878e57cd88";

const FUNDED_AT: u64 = 1_167_987;
const SPENT_AT: u64 = 1_167_995;
const VALUE: u64 = 500_000_000;

fn server() -> LightServer {
    let named = std::env::var("PECU_LIGHT_URL")
        .ok()
        .filter(|url| !url.is_empty());
    match named {
        Some(url) => LightServer::connect(&url, &Network::Testnet),
        None => LightServer::shipped(&Network::Testnet),
    }
    .expect("connect to the light server")
}

fn watching() -> Shielded {
    let bytes: [u8; 128] = hex::decode(DFVK.replace(char::is_whitespace, ""))
        .expect("hex")
        .try_into()
        .expect("128 bytes");
    Shielded::watching(&ShieldedView {
        dfvk: bytes,
        address: "zs1-the-sdks-published-account".to_string(),
        diversifier_index: [0u8; 11],
    })
    .expect("viewing key")
}

/// The note is found on the live chain, at the value and height it really had.
#[test]
#[ignore = "reaches a real lightwalletd; see the module docs"]
fn a_real_note_is_found_by_asking_the_chain() {
    let server = server();
    let mut shielded = watching();

    // The range that contains the payment but not the spend.
    let progress = shielded
        .sync(server.client(), FUNDED_AT, SPENT_AT - 1)
        .expect("scan the live chain");

    println!("scanned {}..={}", progress.from, progress.to);
    println!("balance {}", shielded.balance());
    println!("notes   {}", shielded.note_count());

    assert_eq!(
        shielded.balance(),
        VALUE,
        "the live chain did not yield the note the committed blocks do",
    );
    assert_eq!(shielded.note_count(), 1);
}

/// Scanning on past the spend, the same note is worth nothing.
///
/// The join that a wallet gets wrong by reporting detected notes: it is still
/// detected in this range, and its nullifier is in it too.
#[test]
#[ignore = "reaches a real lightwalletd; see the module docs"]
fn the_same_note_is_spent_eight_blocks_later() {
    let server = server();
    let mut shielded = watching();

    shielded
        .sync(server.client(), FUNDED_AT, SPENT_AT)
        .expect("scan");

    assert_eq!(shielded.balance(), 0, "a spent note was reported as funds");
    assert_eq!(shielded.note_count(), 0);
}

/// A continuation picks up where the first scan stopped, and proves it.
///
/// This is the call a wallet actually lives in — the first scan is a one-off
/// and every one after it is this. It refuses a range that does not descend
/// from the block the last one ended on, which is what makes a reorg loud
/// rather than a quiet shift of every note position after it.
#[test]
#[ignore = "reaches a real lightwalletd; see the module docs"]
fn a_second_scan_continues_the_first_against_the_live_chain() {
    let server = server();
    let mut shielded = watching();

    shielded
        .sync(server.client(), FUNDED_AT, SPENT_AT - 1)
        .expect("first scan");
    assert_eq!(shielded.balance(), VALUE);

    // The tail, which contains the spend.
    let tail = shielded
        .sync(server.client(), FUNDED_AT, SPENT_AT)
        .expect("continuation");

    println!("continued {}..={}", tail.from, tail.to);
    assert_eq!(
        tail.from, SPENT_AT,
        "the tail did not start where it should"
    );
    assert_eq!(tail.rewound, 0, "an unexpected rollback");
    assert_eq!(
        shielded.balance(),
        0,
        "absorbing the tail lost the nullifier that spends the note",
    );
}

/// A different key sees none of it, asked of the same live blocks.
#[test]
#[ignore = "reaches a real lightwalletd; see the module docs"]
fn a_stranger_sees_nothing_on_the_live_chain() {
    let stranger = verus_sdk::light::derive_account(&[9u8; 64], 1, 0).expect("derive");
    let mut shielded = Shielded::watching(&ShieldedView {
        dfvk: stranger.dfvk,
        address: "zs1-a-stranger".to_string(),
        diversifier_index: stranger.diversifier_index,
    })
    .expect("viewing key");

    shielded
        .sync(server().client(), FUNDED_AT, SPENT_AT)
        .expect("scan");

    assert_eq!(shielded.balance(), 0);
    assert_eq!(shielded.note_count(), 0);
}

/// A scan can begin where a wallet with no birthday begins it.
///
/// The wallet starts at Sapling activation, which the server reports as 1 on
/// VRSCTEST — and scanning block 1 needs the commitment tree at height **0**,
/// which cannot be asked for at all: protobuf omits zero-valued fields, so the
/// request arrives with no identifier and lightwalletd says so.
///
/// It shipped that way for one build and failed on the first real run, in front
/// of somebody. This asserts the floor holds.
#[test]
#[ignore = "asks the live chain"]
fn a_scan_can_start_at_the_beginning_of_the_chain() {
    let server = server();
    let activation = server.info().sapling_activation_height;
    println!("sapling activation: {activation}");

    // Exactly what the wallet does, over a short span so the test is quick.
    let start = activation.max(2);
    let mut shielded = watching();
    let progress = shielded
        .sync(server.client(), start, start + 2_000)
        .expect("a scan from the earliest scannable height");

    println!("scanned {}..={}", progress.from, progress.to);
    assert_eq!(progress.from, start);

    // And the height below the floor is genuinely unusable, which is why the
    // floor exists rather than being a superstition.
    let refused = watching().sync(server.client(), 1, 100);
    assert!(
        refused.is_err(),
        "height 0 became requestable; the floor in scan_shielded can go",
    );
}

/// How long a scan actually takes, per thousand blocks.
///
/// Printed rather than asserted: it is a measurement of somebody else's server
/// and this machine's CPU on one afternoon, and a threshold would fail for
/// reasons that are nobody's fault. What it exists for is the birthday
/// question — "what does it cost to scan from Sapling activation" is otherwise
/// answered by guessing, and the guesses are usually an order of magnitude out.
#[test]
#[ignore = "measures scan throughput against the live chain"]
fn how_long_a_scan_takes() {
    use std::time::Instant;

    let server = server();
    let tip = server.synced_height().expect("tip");

    for span in [1_000u64, 5_000, 20_000] {
        let mut shielded = watching();
        let from = tip - span;
        let started = Instant::now();
        let progress = shielded.sync(server.client(), from, tip).expect("scan");
        let took = started.elapsed();

        // Precision is irrelevant here: these are seconds printed for a person
        // to read, and a block height cannot approach the point where an f64
        // stops being exact.
        #[allow(clippy::cast_precision_loss)]
        let per_1k = took.as_secs_f64() / (span as f64 / 1000.0);
        println!(
            "{span:>6} blocks in {:>6.1?}  =  {per_1k:.2}s per 1000  \
             ({}..={})",
            took, progress.from, progress.to,
        );
        // What the same rate means for a wallet with no birthday at all.
        println!(
            "         → Sapling activation to tip ({} blocks) would be {:.0} minutes",
            tip,
            {
                #[allow(clippy::cast_precision_loss)]
                let minutes = (tip as f64 / 1000.0) * per_1k / 60.0;
                minutes
            },
        );
    }
}
