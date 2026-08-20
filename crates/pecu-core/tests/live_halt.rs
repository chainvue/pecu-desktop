//! Reading the consensus circuit breaker off a real node.
//!
//! `#[ignore]`d for the reason the other live tests give. Run it deliberately:
//!
//! ```sh
//! cargo test -p pecu-core --test live_halt -- --ignored --nocapture
//! ```
//!
//! # What this proves that the unit tests cannot
//!
//! `upgrade.rs` parses a descriptor that was pasted into it. This one goes and
//! gets it: the oracle identity, the chain-specific content key, the bytes out
//! of the content multimap, and the tip to judge them against. If the daemon
//! renders the value some other way — structured instead of hex, a different
//! key spelling, several entries — this is what says so.
//!
//! **Read-only.** Two questions and nothing else.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use pecu_chain::{Chain, Network};
use pecu_core::upgrade::{self, Severity};
use verus_sdk::network::ChainReader;

const TESTNET: &str = "https://api.verustest.net";

/// The whole path, end to end, against VRSCTEST.
#[ignore = "talks to api.verustest.net"]
#[test]
fn the_chains_own_oracle_says_whether_conversions_are_being_taken() {
    let chain = Chain::live(TESTNET).expect("a client for the testnet endpoint");
    let oracle = Network::Testnet.oracle().expect("testnet has an oracle");

    let tip = chain.block_count().expect("a tip");
    let content = chain
        .identity_content(oracle.identity)
        .expect("the oracle identity resolves");

    println!("oracle    {}", oracle.identity);
    println!("key       {}", oracle.content_key);
    println!("tip       {tip}");
    println!(
        "map keys  {:?}",
        content.content_multimap.keys().collect::<Vec<_>>()
    );

    let values: Vec<Vec<u8>> = content
        .content_multimap
        .get(oracle.content_key)
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_bytes().map(<[u8]>::to_vec))
                .collect()
        })
        .unwrap_or_default();

    // The daemon may render a value it recognises as structured JSON instead of
    // hex, in which case there are no bytes to read and this whole path is
    // reading nothing. Worth knowing loudly rather than as a quiet `clear`.
    if let Some(entries) = content.content_multimap.get(oracle.content_key) {
        assert_eq!(
            values.len(),
            entries.len(),
            "the daemon returned a structured value, not bytes — `as_bytes` gave nothing",
        );
    }

    for bytes in &values {
        let found = upgrade::parse(bytes).expect("the descriptor parses");
        println!(
            "descriptor version={} daemon={}.{}.{}.{} upgrade={} height={} time={}",
            found.version,
            found.minimum_daemon[0],
            found.minimum_daemon[1],
            found.minimum_daemon[2],
            found.minimum_daemon[3],
            found.upgrade,
            found.activation_height,
            found.activation_time,
        );
    }

    let status = upgrade::read(&values, tip);
    println!(
        "status    {} halted={} in_blocks={} note={}",
        status.severity.label(),
        status.conversions_halted,
        status.in_blocks,
        status.note.code,
    );

    // The one thing that must hold whatever the chain is doing today: a read
    // that succeeded is never `unknown`, because `unknown` means nobody asked.
    assert_ne!(
        status.severity,
        Severity::Unknown,
        "a successful read reported itself as unreachable",
    );

    // And the two states have to agree with each other.
    if status.conversions_halted {
        assert_eq!(status.severity, Severity::Critical);
        assert_eq!(status.in_blocks, 0, "a halt in force is not scheduled");
        assert_eq!(status.note.code, "halt-conversions");
        println!("\nVRSCTEST is not taking conversions. That is why every convert is refused.");
    } else {
        println!("\nVRSCTEST is taking conversions.");
    }
}

/// A chain with no oracle is its own state, and not a clear one.
#[ignore = "talks to api.verustest.net"]
#[test]
fn a_chain_with_no_oracle_does_not_report_itself_healthy() {
    assert_eq!(Network::Other("SOMEPBAAS".to_string()).oracle(), None);

    let unconfigured = upgrade::Status::unconfigured();
    assert!(!unconfigured.conversions_halted);
    assert_eq!(unconfigured.note.code, "halt-unconfigured");

    // And a read that could not happen is louder than one that found nothing.
    assert!(upgrade::Status::unknown().severity > upgrade::Status::clear().severity);
}
