//! The wiring, through the actor: a scan that survives a restart, and a
//! birthday that is only written where it is a fact.
//!
//! `shielded_kept.rs` proves the mechanism — seal, store, restore, and that
//! nothing readable reaches the file. This proves the **core actually uses
//! it**, which is a different claim and the one that silently stops being true.
//!
//! The round trip reaches the network and is `#[ignore]`:
//!
//!   cargo test -p pecu-core --test shielded_persistence -- --ignored --nocapture
//!
//! The second launch is deliberately given **no reachable node and no reachable
//! light server**. Nothing it shows can have come from a chain, so a shielded
//! balance on that run came off the disk or it did not exist.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use pecu_chain::{Network, Node};
use pecu_core::{start, Config};
use pecu_protocol::{Command, Event, Secret, ShieldedFunds, WalletVm};

const PASS: &str = "correct horse battery staple, and then some more";

/// This chain's directory under a home, which is where the store lives.
fn chain_dir(home: &Path) -> PathBuf {
    home.join(Network::Testnet.dir_name())
}

/// A real testnet endpoint, for the run that is allowed to reach one.
fn live_nodes() -> Vec<Node> {
    vec![Node::builtin(0, "public", "https://api.verustest.net")]
}

/// An endpoint that cannot answer, for the run that must not be allowed to.
fn dead_nodes() -> Vec<Node> {
    vec![Node::builtin(0, "nowhere", "https://example.invalid")]
}

/// Wait for a wallet view that satisfies `want`, or give up loudly.
async fn wallet_until(
    events: &mut tokio::sync::mpsc::UnboundedReceiver<Event>,
    what: &str,
    want: impl Fn(&WalletVm) -> bool,
) -> WalletVm {
    let deadline = tokio::time::Instant::now() + Duration::from_mins(5);
    loop {
        let event = tokio::time::timeout_at(deadline, events.recv())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
        match event {
            Some(Event::Wallet(vm)) if want(&vm) => return vm,
            Some(_) => {}
            None => panic!("the core stopped before {what}"),
        }
    }
}

/// Poll for a birthday to be written, or give up loudly.
///
/// Polled rather than awaited on an event: nothing is published when this
/// happens, and adding an event for the benefit of a test would be inventing
/// interface to make an assertion easier.
async fn settled_birthday(store: &pecu_store::Store, label: &str) -> u64 {
    for _ in 0..300 {
        if let Some(height) = store
            .setting(&format!("light_birthday:{label}"))
            .and_then(|height| height.parse().ok())
        {
            assert_eq!(
                store.setting(&format!("light_birthday_pending:{label}")),
                None,
                "a settled birthday must not leave its pending marker behind",
            );
            return height;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    panic!("the birthday was never settled");
}

/// A key generated here gets a birthday; the wallet then keeps what it scans,
/// and a later launch with nothing to ask still knows the answer.
///
/// Three launches in sequence, and they only mean anything in order — the
/// second has to find what the first wrote, and the third has to continue what
/// the second restored. Splitting them into three tests would mean either
/// sharing state between tests or scanning the chain three times, so this is
/// one narrative and `too_many_lines` is allowed for it.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "scans against a real lightwalletd"]
#[allow(clippy::too_many_lines)]
async fn a_scan_is_kept_and_a_later_launch_takes_it_up() {
    let home = tempfile::tempdir().expect("tempdir");
    let handle = tokio::runtime::Handle::current();

    // ── First launch: a node, a light server, and a real scan ───────────────
    let (dispatcher, mut events) = start(
        &handle,
        Config {
            nodes: live_nodes(),
            network: Network::Testnet,
            mock: false,
            home: home.path().to_path_buf(),
        },
    );

    dispatcher.send(Command::CreateWallet {
        name: "kept".to_string(),
        passphrase: Secret::from(PASS),
    });
    let created = wallet_until(&mut events, "the wallet to exist", |vm| {
        !vm.shielded_address.is_empty()
    })
    .await;
    let address = created.shielded_address.clone();
    println!("shielded address  {address}");

    let store = pecu_store::Store::open(&chain_dir(home.path())).expect("store");
    let label = created.keys.first().expect("one key").label.clone();

    dispatcher.send(Command::SetLightServer(
        Network::Testnet
            .light_server()
            .expect("testnet ships one")
            .to_string(),
    ));

    // The birthday is written as soon as a node reports a tip, which is not the
    // moment the key is generated — onboarding is faster than a node probe, and
    // measured against a real endpoint it was faster every time. So this waits
    // for it rather than asserting it is already there, which is the behaviour
    // and not a concession: until it settles, the account is deliberately not
    // scanned at all.
    let birthday: u64 = settled_birthday(&store, &label).await;
    println!("birthday          {birthday}");
    assert!(
        birthday > 1_000_000,
        "the birthday should be near the tip this wallet saw, got {birthday}",
    );

    let scanned = wallet_until(&mut events, "the shielded scan to finish", |vm| {
        matches!(vm.shielded_funds, ShieldedFunds::Scanned(_))
    })
    .await;
    println!("first launch      {:?}", scanned.shielded_funds);

    let sealed = store
        .shielded_scan()
        .expect("the scan was written down after it finished");
    assert!(
        !sealed.contains(&address),
        "the kept scan must not name the address in clear",
    );
    drop(store);
    drop(dispatcher);
    drop(events);

    // ── Second launch: nothing it can ask ───────────────────────────────────
    //
    // The stored light server is still set and still unreachable from here, and
    // the node is `example.invalid`. Anything shielded that appears on this run
    // came off the disk.
    let (dispatcher, mut events) = start(
        &handle,
        Config {
            nodes: dead_nodes(),
            network: Network::Testnet,
            mock: false,
            home: home.path().to_path_buf(),
        },
    );

    dispatcher.send(Command::Unlock {
        passphrase: Secret::from(PASS),
    });

    let restored = wallet_until(&mut events, "the kept scan to be taken up", |vm| {
        vm.shielded_address == address && matches!(vm.shielded_funds, ShieldedFunds::Scanned(_))
    })
    .await;

    println!("second launch     {:?}", restored.shielded_funds);
    assert_eq!(
        restored.shielded_funds, scanned.shielded_funds,
        "the second launch must show what the first one found, not a fresh nothing",
    );
    drop(dispatcher);
    drop(events);

    // ── Third launch: a real server, and therefore a real continuation ──────
    //
    // The one the wallet actually got wrong. A restored scan asked its next
    // stride for blocks 2..=50002 — one stride past the bottom of the chain —
    // and reported the refusal as though the server were at fault:
    //
    //   the light server is behind: it has 50002, and this wallet has scanned
    //   to 1200172
    //
    // So this checks for the absence of that complaint, which means watching
    // for a while rather than waiting for something to arrive.
    let (dispatcher, mut events) = start(
        &handle,
        Config {
            nodes: live_nodes(),
            network: Network::Testnet,
            mock: false,
            home: home.path().to_path_buf(),
        },
    );

    dispatcher.send(Command::Unlock {
        passphrase: Secret::from(PASS),
    });

    let watch_until = tokio::time::Instant::now() + Duration::from_secs(45);
    let mut still_scanned = false;
    while let Ok(Some(event)) = tokio::time::timeout_at(watch_until, events.recv()).await {
        match event {
            Event::Notice(notice) if notice.message.code.starts_with("shielded-scan") => {
                panic!(
                    "a continuation complained: {} / {:?}",
                    notice.message.code, notice.detail,
                );
            }
            Event::Wallet(vm) if matches!(vm.shielded_funds, ShieldedFunds::Scanned(_)) => {
                still_scanned = true;
            }
            _ => {}
        }
    }

    assert!(
        still_scanned,
        "the third launch never reported a scanned balance at all",
    );
    println!("third launch      continued without complaint");
}

/// An imported phrase gets no birthday, however tempting it looks.
///
/// The same words may have been in another wallet for years. When *this* wallet
/// derived the account says nothing about when the account was first paid, and
/// writing the tip here is how a wallet skips past somebody's money — which it
/// has done: 10 VRSCTEST, sixty-three blocks the wrong side of the line.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a node to have reported a tip"]
async fn an_imported_phrase_is_given_no_birthday() {
    let home = tempfile::tempdir().expect("tempdir");
    let handle = tokio::runtime::Handle::current();

    let (dispatcher, mut events) = start(
        &handle,
        Config {
            nodes: live_nodes(),
            network: Network::Testnet,
            mock: false,
            home: home.path().to_path_buf(),
        },
    );

    // A published BIP-39 test vector. It is in every wallet test suite there
    // is, so it is not anybody's, and the point here is what is *not* written.
    dispatcher.send(Command::ImportKey {
        label: "restored".to_string(),
        material: pecu_protocol::ImportMaterial::Phrase(Secret::from(
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon \
             abandon abandon about",
        )),
        passphrase: Secret::from(PASS),
    });

    let imported = wallet_until(&mut events, "the imported key to exist", |vm| {
        vm.keys.iter().any(|key| key.label == "restored")
    })
    .await;
    assert!(!imported.keys.is_empty());

    let store = pecu_store::Store::open(&chain_dir(home.path())).expect("store");
    assert_eq!(
        store.setting("light_birthday:restored"),
        None,
        "an imported phrase must not be told when its account was created — nobody knows",
    );
    // Nor a pending one, which would settle into a height a few seconds later
    // and be exactly the same mistake with a delay on it.
    assert_eq!(
        store.setting("light_birthday_pending:restored"),
        None,
        "an imported phrase must not have a birthday waiting to be invented for it",
    );
}
