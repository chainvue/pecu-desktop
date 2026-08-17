//! Claiming a name and defining a currency under it, across a restart.
//!
//! # What this is really about
//!
//! Two transactions with a wait in the middle, and the wait can outlive the
//! application. The first one costs money — a hundred coins on VRSCTEST — and
//! buys an identity whose only purpose is the currency that was configured in
//! the same breath. If the wallet forgets what that identity was for, somebody
//! is left holding a name they paid for, which **can never be used for a
//! different currency**.
//!
//! So the currency is written down before the registration is broadcast, and a
//! wallet started afterwards has to find it and say so.
//!
//! The scripted chain refuses every broadcast, which is convenient here for the
//! same reason `demo_chain.rs` says: it produces exactly the state this is
//! about — an intent written down with the send having gone nowhere.

#![cfg(feature = "mock")]
#![allow(clippy::expect_used, clippy::panic)]

use pecu_protocol::{Command, Event};

/// The scripted chain's one identity a currency can be defined under.
const LAUNCHABLE: &str = "maker.VRSCTEST@";

fn config(home: std::path::PathBuf) -> pecu_core::Config {
    pecu_core::Config {
        nodes: vec![pecu_chain::Node::builtin(
            0,
            "Scripted chain",
            "mock://scripted",
        )],
        network: pecu_chain::Network::Testnet,
        mock: true,
        home,
    }
}

fn draft(name: &str) -> pecu_protocol::CurrencyDraft {
    pecu_protocol::CurrencyDraft {
        kind: "token".to_string(),
        // Empty: there is no identity yet, and that is the whole point of this
        // path. Core refuses a draft that names both.
        identity: String::new(),
        new_name: name.to_string(),
        mintable: true,
        start_delay: "20".to_string(),
        reserves: Vec::new(),
        preallocations: Vec::new(),
    }
}

/// A node has to have answered before anything can be built: a claim needs a
/// spend permit, and a permit needs a node that has said which chain it is on.
async fn wait_for_a_node(events: &mut tokio::sync::mpsc::UnboundedReceiver<Event>) {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            match events.recv().await {
                Some(Event::Network(vm)) if vm.effective.is_some() => break,
                Some(_) => {}
                None => panic!("the core stopped before a node answered"),
            }
        }
    })
    .await
    .expect("a node answers within thirty seconds");
}

async fn wait_for_pending(
    events: &mut tokio::sync::mpsc::UnboundedReceiver<Event>,
    ready: impl Fn(Option<&pecu_protocol::LaunchPendingVm>) -> bool,
) -> pecu_protocol::LaunchPendingVm {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            match events.recv().await {
                Some(Event::LaunchPending(pending)) if ready(pending.as_deref()) => {
                    break pending.map(|boxed| *boxed).expect("a pending launch");
                }
                Some(_) => {}
                None => panic!("the core stopped before the launch was reported"),
            }
        }
    })
    .await
    .expect("a pending launch is reported within thirty seconds")
}

/// The decision survives the process that made it.
#[tokio::test(flavor = "multi_thread")]
async fn a_currency_waiting_for_its_name_outlives_the_wallet_that_configured_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().to_path_buf();

    let (dispatcher, mut events) =
        pecu_core::start(&tokio::runtime::Handle::current(), config(home.clone()));

    dispatcher.send(Command::CreateWallet {
        name: "demo".to_string(),
        passphrase: pecu_protocol::Secret::from("correct-horse-battery-staple-9931"),
    });
    dispatcher.send(Command::ProbeNodes);
    wait_for_a_node(&mut events).await;

    dispatcher.send(Command::StartCurrencyFromNewName {
        revocation_authority: String::new(),
        recovery_authority: String::new(),
        draft: draft("livecoin"),
    });

    let pending = wait_for_pending(&mut events, |pending| pending.is_some()).await;
    assert_eq!(pending.identity, "livecoin@");
    assert_eq!(pending.step, "awaiting-identity");
    assert!(
        !pending.can_continue,
        "a currency whose name is not on the chain offered to be defined",
    );
    // The diagram is progress, not a plan: the name is chosen and the
    // registration is out, so the wait is where this is.
    assert_eq!(
        pending
            .steps
            .iter()
            .filter(|step| step.state == "done")
            .count(),
        2,
        "the diagram does not agree with the step: {:?}",
        pending.steps,
    );

    // On disk, and written before the registration was broadcast — which on
    // this chain never succeeds at all, so its existence here is exactly the
    // ordering this test is about.
    //
    // Where on disk is the layout's decision, so it is asked for: a hardcoded
    // path would keep passing while reading a file nothing writes to.
    let path =
        pecu_core::paths::Paths::new(home.clone(), &pecu_chain::Network::Testnet, true)
            .launch();
    let text = std::fs::read_to_string(&path).expect("the currency was written down");
    assert!(
        text.contains("livecoin@"),
        "the record does not name the identity it is waiting for: {text}",
    );
    assert!(
        text.contains("token"),
        "the record does not carry what was configured: {text}",
    );

    // The process ends here.
    dispatcher.send(Command::Shutdown);
    drop(dispatcher);

    // A new one, on the same directory.
    let (_resumed, mut events) =
        pecu_core::start(&tokio::runtime::Handle::current(), config(home));

    let resumed = wait_for_pending(&mut events, |pending| pending.is_some()).await;
    assert_eq!(
        resumed.identity, "livecoin@",
        "a wallet restarted mid-launch did not pick the currency up",
    );
    assert_eq!(resumed.step, "awaiting-identity");
}

/// Resuming once the name is on the chain: signed, and waiting for a press.
///
/// # Why the record is written by `Intent` and not by hand
///
/// Because the file format is not the subject. What is being tested is that a
/// record the wallet's own code wrote is one the wallet's own code can pick up
/// and turn into a signed launch — and a hand-written file would test this
/// against a shape nothing produces.
///
/// The identity is the scripted chain's `maker@`, which exists, defines no
/// currency and is controlled by this wallet's key. That is what makes the
/// launch buildable here at all.
#[tokio::test(flavor = "multi_thread")]
async fn a_resumed_launch_is_signed_but_waits_for_a_press() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().to_path_buf();

    // The wallet first, because the intent names the key that funds it and the
    // key does not exist until there is a wallet.
    let (dispatcher, mut events) =
        pecu_core::start(&tokio::runtime::Handle::current(), config(home.clone()));
    dispatcher.send(Command::CreateWallet {
        name: "demo".to_string(),
        passphrase: pecu_protocol::Secret::from("correct-horse-battery-staple-9931"),
    });
    dispatcher.send(Command::ProbeNodes);
    wait_for_a_node(&mut events).await;
    dispatcher.send(Command::Shutdown);
    drop(dispatcher);

    // A launch that got as far as its identity landing, written by the code
    // that writes it.
    let path =
        pecu_core::paths::Paths::new(home.clone(), &pecu_chain::Network::Testnet, true)
            .launch();
    let mut intent = pecu_core::launch::Intent::open(path);
    let mut record = draft("maker");
    record.new_name = String::new();
    intent.begin(LAUNCHABLE, "Key 1", record);
    intent.identity_exists();

    // A new process, finding it.
    let (dispatcher, mut events) =
        pecu_core::start(&tokio::runtime::Handle::current(), config(home));
    dispatcher.send(Command::Unlock {
        passphrase: pecu_protocol::Secret::from("correct-horse-battery-staple-9931"),
    });
    dispatcher.send(Command::ProbeNodes);

    // Both, in one loop, because the order is not this test's to assume — and
    // getting that wrong is not a failure, it is a *pass on nothing*: the
    // launch is reported in the startup prologue, before any node has answered,
    // so a wait-for-the-node-first loop reads past it and then times out
    // waiting for something already sent.
    let mut pending: Option<pecu_protocol::LaunchPendingVm> = None;
    let mut answered = false;
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while pending.is_none() || !answered {
            match events.recv().await {
                Some(Event::LaunchPending(Some(vm))) if vm.step == "ready" => {
                    pending = Some(*vm);
                }
                Some(Event::Network(vm)) if vm.effective.is_some() => answered = true,
                Some(_) => {}
                None => panic!("the core stopped before it reported the launch"),
            }
        }
    })
    .await
    .expect("the unfinished launch and a node both arrive within thirty seconds");

    let pending = pending.expect("the loop only ends with one");
    assert!(
        pending.can_continue,
        "an identity that exists did not offer to have its currency defined",
    );
    assert_eq!(pending.identity, LAUNCHABLE);

    // Nothing has been signed yet. **This is the assertion that matters most in
    // this file**: opening an application is not consent to spend two hundred
    // coins, so the wallet waits here however long it takes.
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                match events.recv().await {
                    Some(Event::LaunchPrepared(Some(_))) => break,
                    Some(_) => {}
                    // Distinct from the break above, and deliberately loud: a
                    // core that stopped would also end this loop, and letting
                    // that read as "it did not sign" would turn a dead actor
                    // into a passing assertion about restraint.
                    None => panic!("the core stopped while nothing was supposed to happen"),
                }
            }
        })
        .await
        .is_err(),
        "a restarted wallet signed a launch nobody asked it to",
    );

    // What the application does on its own between unlocking and anybody
    // reaching this screen: read the balance, which is where the chain's own
    // currency comes from, and read the identities.
    //
    // Both are needed and neither is instant. The record holds a *name*, not an
    // address — the address did not exist when the form was filled in — so the
    // identity list is where the address comes from; and a definition needs the
    // parent currency, which is a fact about the chain nobody has asked for yet
    // in a freshly started process. Pressing before either lands is refused,
    // correctly, which is what this test proved on the way here.
    dispatcher.send(Command::Refresh(pecu_protocol::RefreshScope::All));
    dispatcher.send(Command::RefreshIdentities);
    let mut listed = false;
    let mut funded = false;
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while !listed || !funded {
            match events.recv().await {
                Some(Event::Identities { yours, .. }) => {
                    listed = yours.iter().any(|row| row.name == LAUNCHABLE);
                }
                Some(Event::Portfolio(_)) => funded = true,
                Some(_) => {}
                None => panic!("the core stopped before the identities arrived"),
            }
        }
    })
    .await
    .expect("the identities and the balance both arrive within thirty seconds");

    // And now somebody presses it.
    dispatcher.send(Command::ResumeLaunch);

    // Either outcome, so a refusal is reported as what it said rather than as
    // thirty seconds of nothing.
    let review = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            match events.recv().await {
                Some(Event::LaunchPrepared(Some(review))) => break *review,
                Some(Event::Notice(notice)) => {
                    panic!("resuming was refused: {} — {}", notice.title, notice.detail)
                }
                Some(_) => {}
                None => panic!("the core stopped before the launch was built"),
            }
        }
    })
    .await
    .expect("a resumed launch is built within thirty seconds");

    assert_eq!(review.name, "maker");
    assert!(
        !review.fee_display.is_empty() && !review.start_block.is_empty(),
        "the review is missing the figures it exists to show: {review:?}",
    );
}
