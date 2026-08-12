//! Against a real public node.
//!
//! `#[ignore]`d: the rest of the suite must pass on a machine with no network,
//! and a test that fails because someone's wifi dropped teaches people to
//! ignore red builds.
//!
//! ```sh
//! cargo test -p chainvue-chain --test live_probe -- --ignored --nocapture
//! ```
//!
//! Read-only. Nothing here can spend: `probe` calls `chain_info()` and the
//! whole file never constructs a `SpendPermit`, without which there is no
//! broadcaster to reach.

use chainvue_chain::{Network, Node, NodeStatus};

const TESTNET: &str = "https://api.verustest.net";

#[test]
#[ignore = "needs the network; run explicitly"]
fn a_public_testnet_node_answers_and_says_which_chain_it_is() {
    let (result, latency) = chainvue_chain::probe(TESTNET);
    let info = result.unwrap_or_else(|e| panic!("{TESTNET} did not answer: {e}"));

    println!("chain   {}", info.name);
    println!("blocks  {}", info.blocks);
    println!("version {}", info.version);
    println!("latency {} ms", latency.as_millis());

    // The property the whole node manager rests on: the chain is whatever the
    // node SAYS, and here it must say VRSCTEST.
    assert_eq!(
        Network::from_chain_name(&info.name),
        Network::Testnet,
        "expected VRSCTEST from {TESTNET}, got `{}`",
        info.name,
    );
    assert!(info.blocks > 1_000_000, "implausible tip: {}", info.blocks);

    let mut node = Node::builtin(0, "live", TESTNET);
    node.record_success(&info, latency, &Network::Testnet);
    assert_eq!(node.status, NodeStatus::Online);
    assert!(node.latency_ms().is_some());
}

/// A URL that resolves to nothing must land as `Offline` with a backoff, not
/// as a panic and not as a hang.
#[test]
#[ignore = "needs the network; run explicitly"]
fn an_unreachable_endpoint_backs_off_instead_of_hanging() {
    let (result, elapsed) = chainvue_chain::probe("https://node.invalid.chainvue.test");
    let error = result.expect_err("a nonexistent host must not answer");

    let mut node = Node::builtin(0, "dead", "https://node.invalid.chainvue.test");
    node.record_failure(&error);

    assert_eq!(node.consecutive_failures, 1);
    assert!(
        node.retry_after.is_some(),
        "a failure must schedule a retry"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "the probe timeout is 5 s; this took {elapsed:?}",
    );
}
