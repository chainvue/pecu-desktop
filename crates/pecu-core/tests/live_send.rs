//! Sending a real payment on a real chain, through the wallet's own path.
//!
//! # Why this exists when `send_build.rs` already passes
//!
//! Because that test builds and signs against a scripted chain and asserts —
//! correctly — that **nothing was broadcast**. Every check it makes is about a
//! transaction that never left the process. The one property it structurally
//! cannot test is the only one that matters at the end: that a node accepts
//! what this wallet produces.
//!
//! The registration of `maker@` on VRSCTEST proved the broadcast *machinery*
//! works, since an identity goes out through the same `SpendPermit` →
//! `Chain::broadcaster` → `Unsent::broadcast` path. It did not prove a payment
//! does. This closes that.
//!
//! # It spends money, so it is gated twice
//!
//! `#[ignore]`, and then refused unless **both** environment variables are set:
//!
//! ```sh
//! export PECU_LIVE_SEND=1
//! export PECU_LIVE_WIF=<a funded VRSCTEST WIF>      # in your own shell
//! cargo test -p pecu-core --test live_send -- --ignored --nocapture
//! ```
//!
//! Two variables rather than one because a WIF sitting in an environment is not
//! by itself consent to spend from it — every other live test in this workspace
//! is read-only and would happily run with one set.
//!
//! **It pays the signer's own address.** Nothing can be lost but the fee, and
//! the point is whether the network accepts the transaction, not where the
//! money goes. A recipient would only add a way to get this wrong.
//!
//! Nothing here prints the key, and nothing writes it anywhere: the vault it
//! builds lives in a temporary directory that is removed when the test ends.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use pecu_chain::{Chain, Network, Node, NodeManager};
use pecu_core::send;
use pecu_keystore::{NewKey, Vault};
use pecu_protocol::Secret;
use verus_sdk::money::Amount;
use verus_sdk::network::ChainReader;
use verus_sdk::verus_keys::PrivateKey;

const TESTNET: &str = "https://api.verustest.net";
/// What goes out. Small enough to be nothing, large enough to be above dust.
const AMOUNT_SATS: u64 = 10_000;
const LABEL: &str = "live";

/// The key, or `None` with a reason printed.
///
/// Returned rather than `expect`ed so the ordinary case — somebody running the
/// whole suite — is a skip with an explanation, not a failure.
fn funded_key() -> Option<PrivateKey> {
    if std::env::var("PECU_LIVE_SEND").as_deref() != Ok("1") {
        eprintln!(
            "skipping: this test spends. Set PECU_LIVE_SEND=1 and PECU_LIVE_WIF=<funded WIF>."
        );
        return None;
    }
    let wif =
        std::env::var("PECU_LIVE_WIF").expect("PECU_LIVE_SEND is set but PECU_LIVE_WIF is not");
    Some(PrivateKey::from_wif(&wif).expect("PECU_LIVE_WIF is not a WIF this chain can read"))
}

/// A vault holding that one key, in a directory that does not outlive the test.
fn vault_holding(key: PrivateKey) -> (tempfile::TempDir, Vault) {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let passphrase = Secret::new("a passphrase that exists for the length of this test".into());
    let vault =
        Vault::create(&dir.path().join("vault.json"), "live", &passphrase).expect("a fresh vault");
    vault
        .unlock(&passphrase)
        .expect("the passphrase we just set");
    vault
        .add_key(LABEL, NewKey::FromWif { key })
        .expect("importing a WIF");
    (dir, vault)
}

/// A node manager that will actually issue a permit for this node.
///
/// Built by hand because the permit is not a formality: it refuses an unknown
/// network, a mismatched one, a syncing node and mainnet-without-consent. The
/// only way to get one is to put a node into the state where all of that is
/// true, which is what `record_success` does with a real `getinfo`.
fn permitted(chain: &Chain) -> (NodeManager, u32) {
    let (info, latency) = chain.probe();
    let info = info.expect("the testnet endpoint answered getinfo");
    assert_eq!(
        info.name, "VRSCTEST",
        "this test only ever runs against the testnet"
    );

    let mut nodes = NodeManager::new(
        vec![Node::builtin(0, "VRSCTEST (public)", TESTNET)],
        Network::Testnet,
    );
    nodes
        .get_mut(0)
        .expect("the node just added")
        .record_success(&info, latency, &Network::Testnet);

    (nodes, info.blocks)
}

/// The whole path: validate, build, sign, review the signed bytes, broadcast,
/// and then ask the chain whether it has it.
#[ignore = "spends real coins on VRSCTEST"]
#[test]
fn a_payment_built_by_this_wallet_is_accepted_by_the_network() {
    let Some(key) = funded_key() else { return };

    let address = key.address().to_string();
    let chain = Chain::live(TESTNET).expect("a client for the testnet endpoint");
    let (nodes, tip) = permitted(&chain);

    // What can actually be spent, before anything is built. A test that fails
    // with "insufficient funds" three steps later has wasted everybody's time
    // and looks like a bug in the builder.
    let funding = verus_sdk::network::spendable(&chain, &address).expect("the funding read");
    println!("address   {address}");
    println!("tip       {tip}");
    println!("spendable {}", funding.total.to_coins_string());
    if funding.total.to_sat() < AMOUNT_SATS * 4 {
        eprintln!(
            "skipping: {} has {} and this needs a little over {}. Fund it and run again.",
            address,
            funding.total.to_coins_string(),
            Amount::from_sat(AMOUNT_SATS).to_coins_string(),
        );
        return;
    }

    let (_dir, vault) = vault_holding(key);

    // Paying itself, deliberately — see the module docs.
    let draft = pecu_protocol::SendDraft {
        // The transparent route, which is what this test has always exercised.
        from_pool: pecu_protocol::Pool::Transparent,
        from_label: LABEL.to_string(),
        to: address.clone(),
        amount: Amount::from_sat(AMOUNT_SATS).to_coins_string(),
        // A fixed small amount against a real chain — emptying the funded key
        // would leave nothing for the next run.
        send_all: false,
    };

    let verdict = send::validate(&draft, funding.total);
    assert!(verdict.ready, "the form refused its own draft: {verdict:?}");

    // No second source: this test talks to one live endpoint, which is the
    // shape a default install is in. Corroboration is exercised offline in
    // `send_corroboration.rs`, where both nodes can be scripted.
    let prepared =
        send::prepare(&chain, None, &vault, LABEL, &draft, "").expect("the build succeeds");

    // The review, from the signed bytes, before anything is sent. Printed
    // because it is what a person would have read on the screen at this point.
    let review = send::review(1, &prepared, &address, funding.total, true);
    println!("fee       {}", review.fee_display);
    println!("total     {}", review.total_display);
    for output in &review.outputs {
        println!(
            "  out {:<36} {:>18}  change={}",
            output.address.as_deref().unwrap_or("<unreadable>"),
            output.amount_display,
            output.is_change,
        );
    }
    assert!(
        review
            .outputs
            .iter()
            .all(|o| o.kind.code != "output-unreadable"),
        "an output of a transaction about to be signed could not be read",
    );

    let permit = nodes
        .spend_permit()
        .expect("a healthy testnet node must yield a permit");

    // The line that spends.
    let sent = send::broadcast(&chain, &permit, prepared).expect("the network accepts it");
    println!("txid      {}", sent.txid);
    println!("fee paid  {}", sent.fee.to_coins_string());

    // And the chain's own word for it, which is the only evidence that counts.
    let seen = chain
        .confirmations(&sent.txid)
        .expect("asking the node about a txid it just took");
    assert!(
        seen.is_some(),
        "the node accepted {} and then did not know it",
        sent.txid,
    );
    println!("the chain has it: {seen:?} confirmations");
}
