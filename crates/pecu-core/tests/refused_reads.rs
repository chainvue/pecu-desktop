//! Every screen that reads the chain is behind the same refusal, not just the
//! dashboard.
//!
//! # What this is the regression test for
//!
//! `reading_refused` decides that the active node must not be read from — it is
//! on another chain, or its own two claims about which chain it is on disagree —
//! and it had exactly one caller, `Core::refresh`. `Command::RefreshMarkets` and
//! `Command::RefreshIdentities` do not go through `refresh`; they reach
//! `refresh_markets` and `refresh_identities` directly. So the dashboard refused
//! a node and the Markets screen, on the very next command, read prices from it.
//!
//! The gate was one path wide for as long as it existed. That is the shape of
//! defect a test of the rule cannot find, because the rule was never wrong:
//! these tests go through the actor's own command queue, which is the only place
//! the omission is visible.
//!
//! # How a refusal is arranged without a network
//!
//! The scripted chain answers `getinfo` as VRSCTEST with testnet's pinned chain
//! id — an honest node, correctly identified. Pointing a **mainnet** wallet at
//! it is therefore the wrong-chain refusal in its purest form: nothing is
//! lying, the wallet is simply asking a chain about addresses that are not on
//! it. `Node::record_success` files that as `WrongNetwork`, and
//! `reading_refused` turns it into `read-wrong-chain`.
//!
//! # There is deliberately no wallet in these tests
//!
//! Creating one would unlock it, and an unlocked wallet makes `Core::refresh`
//! reach the gate on its own — so a notice would arrive whether or not the two
//! commands under test were gated, and the test would pass against the bug.
//! Without a wallet, `refresh` returns at its locked check, above the gate, and
//! the only thing in the process that can emit `wrong_chain` is the code these
//! tests are about. Both of them fail on the parent commit for the right reason:
//! nothing is emitted at all.

#![cfg(feature = "mock")]
#![allow(clippy::expect_used, clippy::panic)]

use pecu_protocol::{Command, Event, Severity};

/// How long to wait for the actor to answer. The scripted chain sleeps on every
/// read to make loading states reachable, so this is generous rather than tight;
/// it exists to fail the test instead of hanging CI.
const PATIENCE: std::time::Duration = std::time::Duration::from_secs(30);

/// The wallet asks about mainnet; the scripted chain answers about testnet.
#[tokio::test(flavor = "multi_thread")]
async fn markets_are_not_read_from_a_node_the_refresh_path_refuses() {
    let refusal = refusal_for(Command::RefreshMarkets).await;

    assert_eq!(refusal.code, "wrong_chain");
    assert_eq!(refusal.message.code, "read-wrong-chain");
    // The chain the node is on, then the chain the wallet was asked for — the
    // order the sentence in `note.slint` reads them in.
    assert_eq!(refusal.message.args, ["Testnet", "Mainnet"]);
    assert_eq!(refusal.severity, Severity::Warning);
}

/// The same refusal, from the other command that skips `refresh`.
#[tokio::test(flavor = "multi_thread")]
async fn identities_are_not_read_from_a_node_the_refresh_path_refuses() {
    let refusal = refusal_for(Command::RefreshIdentities).await;

    assert_eq!(refusal.code, "wrong_chain");
    assert_eq!(refusal.message.code, "read-wrong-chain");
    assert_eq!(refusal.message.args, ["Testnet", "Mainnet"]);
}

/// `RefreshCurrencies` is the third way into `refresh_identities`, and it is
/// worth its own case: it is a different command arm, and a gate written at the
/// call sites rather than inside the function is exactly what would leave this
/// one open.
#[tokio::test(flavor = "multi_thread")]
async fn the_currency_walk_is_refused_by_the_same_gate() {
    assert_eq!(
        refusal_for(Command::RefreshCurrencies).await.code,
        "wrong_chain"
    );
}

/// And the control: on the chain it was asked for, the same command reads.
///
/// Without this, a gate that refused unconditionally would pass every test
/// above — and a Markets screen that is always empty is not the fix.
#[tokio::test(flavor = "multi_thread")]
async fn a_node_on_the_requested_chain_is_still_read_from() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (dispatcher, mut events) = scripted_wallet(&dir, pecu_chain::Network::Testnet);

    dispatcher.send(Command::ProbeNodes);
    dispatcher.send(Command::RefreshMarkets);

    let rows = tokio::time::timeout(PATIENCE, async {
        loop {
            match events.recv().await {
                Some(Event::Markets { rows, .. }) => break rows,
                Some(Event::Notice(error)) if error.code == "wrong_chain" => {
                    panic!("a node on the requested chain was refused");
                }
                Some(_) => {}
                None => panic!("the core stopped before pricing anything"),
            }
        }
    })
    .await
    .expect("a market book within thirty seconds");

    assert!(
        !rows.is_empty(),
        "the scripted chain priced nothing, so this control proves nothing",
    );
}

/// Run one command against a refused node and hand back the notice it produced.
///
/// The two commands are sent in order down one unbounded channel and
/// `Command::ProbeNodes` is awaited inside the actor, so the node has recorded
/// what it is before the command under test is handled. No sleep, and no
/// dependence on the fifteen-second tip poll.
async fn refusal_for(command: Command) -> pecu_protocol::UiError {
    let dir = tempfile::tempdir().expect("tempdir");
    let (dispatcher, mut events) = scripted_wallet(&dir, pecu_chain::Network::Mainnet);

    dispatcher.send(Command::ProbeNodes);
    dispatcher.send(command);

    tokio::time::timeout(PATIENCE, async {
        loop {
            match events.recv().await {
                Some(Event::Notice(error)) => break error,
                Some(_) => {}
                None => panic!("the core stopped without refusing anything"),
            }
        }
    })
    .await
    .expect("a refusal within thirty seconds")
}

/// A core pointed at the scripted chain, asking about `network`.
///
/// The URL is one nothing in this application knows how to dial, so if mock mode
/// ever stops intercepting, this fails as a connection error rather than quietly
/// reading a real endpoint — the same guard `demo_chain.rs` sets.
fn scripted_wallet(
    dir: &tempfile::TempDir,
    network: pecu_chain::Network,
) -> (
    pecu_core::Dispatcher,
    tokio::sync::mpsc::UnboundedReceiver<Event>,
) {
    pecu_core::start(
        &tokio::runtime::Handle::current(),
        pecu_core::Config {
            nodes: vec![pecu_chain::Node::builtin(
                0,
                "Scripted chain",
                "mock://scripted",
            )],
            network,
            mock: true,
            home: dir.path().to_path_buf(),
        },
    )
}
