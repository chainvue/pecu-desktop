//! The demo build reads a whole dashboard, and reads it from nothing.
//!
//! # Why this test exists
//!
//! `--features mock` shipped for a long time as a *banner*. The flag reached the
//! UI, the mock crate was written and tested, and in between the two nothing
//! connected them: the core built a live client on every path, and every probe
//! dialled `api.verustest.net`. A build advertising itself as scripted was
//! making real requests to a real endpoint and showing real balances, which is
//! the opposite of both halves of what it claimed.
//!
//! Nothing caught that, because every test of the mock chain asked the mock
//! chain directly. This one goes through the same function the dashboard does.
//!
//! # And why it asserts three different numbers
//!
//! A wallet showing one balance cannot be told apart from a correct one until
//! spendable, maturing and pending differ. The script is built so they do — and
//! the SDK decides which is which, by reading the output ages and asking whether
//! each young output's transaction is a coinbase. So this is also the test that
//! the scripted chain answers those questions the way a daemon does.

#![cfg(feature = "mock")]
#![allow(clippy::expect_used, clippy::panic)]

use chainvue_chain::Chain;
use chainvue_core::portfolio;
use verus_sdk::verus_keys::PrivateKey;

/// An address derived from a fixed scalar rather than typed out.
///
/// A hand-written address has a checksum, and getting it wrong fails the test
/// for a reason that has nothing to do with what is being tested.
fn address(scalar: u8) -> String {
    PrivateKey::from_bytes(&[scalar; 32], true)
        .expect("a fixed scalar is a valid key")
        .address()
        .to_string()
}

const COIN: i64 = 100_000_000;

#[test]
fn the_demo_chain_fills_a_dashboard_with_no_network() {
    let funded = address(11);
    let empty = address(23);
    let chain = Chain::mock(&[funded.clone(), empty.clone()]).expect("the demo script builds");

    let reading = portfolio::read(
        &chain,
        &[funded.clone(), empty.clone()],
        portfolio::Cached::default(),
    );

    assert!(
        reading.failure.is_none(),
        "the scripted chain failed a read: {:?}",
        reading.failure,
    );

    let spendable = sats(reading.spendable);
    let immature = sats(reading.immature);

    // ── The three balances, and they are three ──────────────────────────
    assert_eq!(
        spendable,
        415 * COIN + COIN / 4,
        "spendable is not the four confirmed receipts",
    );
    assert_eq!(
        immature,
        112 * COIN + COIN / 2,
        "the mined output eighty blocks back is not being withheld — either the \
         script stopped calling it a coinbase, or maturity stopped being applied",
    );
    assert_eq!(
        sats(reading.pending_in),
        5 * COIN,
        "the unconfirmed receipt is missing, so the mempool is not being read",
    );
    assert_ne!(
        spendable, immature,
        "a demo where every number is the same number demonstrates nothing",
    );

    // ── History, which is what the chart is drawn from ───────────────────
    let history = reading.history.expect("the activity list loaded");
    assert_eq!(
        history.len(),
        5,
        "the activity list did not get the five movements",
    );

    // The confirmed balance and the movements that produced it have to agree:
    // the chart walks backwards from the balance applying each delta, so a
    // script where they disagree draws a series that never reaches zero.
    let moved: i64 = history.iter().map(|row| row.net_native.to_sat()).sum();
    assert_eq!(
        moved,
        spendable + immature,
        "the movements do not sum to the confirmed balance",
    );
}

/// Satoshis as a signed count, for comparing against a written-out figure.
fn sats(amount: verus_sdk::money::Amount) -> i64 {
    i64::try_from(amount.to_sat()).expect("a demo figure that fits in an i64")
}

/// The demo build gets all the way to a signed transaction, and no further.
///
/// This is the half of the send flow worth having: coin selection, the fee, the
/// signature and the decode that the review screen is drawn from all run for
/// real against the scripted chain, because the outputs carry real
/// pay-to-public-key-hash scripts. What it must not do is claim the payment went
/// anywhere — and it cannot, because the scripted chain has nothing to send it
/// with and the SDK would refuse a made-up id if it tried.
#[test]
fn the_demo_build_signs_a_real_payment_and_still_cannot_send_it() {
    let key = PrivateKey::from_bytes(&[11u8; 32], true).expect("a fixed scalar is a valid key");
    let from = key.address().to_string();
    let chain = Chain::mock(std::slice::from_ref(&from)).expect("the demo script builds");

    let unsent = verus_flows::prepare_send(
        &chain,
        &key,
        &address(23),
        verus_sdk::money::Amount::from_sat(10 * 100_000_000),
    )
    .expect("the demo build can select coins, build and sign");

    // A permit, because there is no other route to a broadcaster. The scripted
    // chain reports the network the wallet is set to, so the guard is satisfied
    // — the refusal below is not the permit refusing, it is the chain having
    // nothing to send with.
    let mut nodes = chainvue_chain::NodeManager::new(
        vec![chainvue_chain::Node::builtin(
            0,
            "Scripted chain",
            "mock://scripted",
        )],
        chainvue_chain::Network::Testnet,
    );
    let (info, latency) = chain.probe();
    let info = info.expect("the scripted chain answers");
    nodes
        .get_mut(0)
        .expect("the node just added")
        .record_success(&info, latency, &chainvue_chain::Network::Testnet);
    let permit = nodes.spend_permit().expect("the guard is satisfied");

    let error = unsent
        .broadcast(&chain.broadcaster(&permit))
        .expect_err("the scripted chain broadcast a transaction");
    assert!(
        !matches!(error, verus_flows::FlowError::BroadcastUncertain { .. }),
        "a scripted send reported an unknown outcome, which sends the wallet \
         down the resolve-then-resend path against a chain that does not exist",
    );
}

/// The whole way through: a wallet is made, unlocked, refreshed, and the
/// dashboard arrives — with no node anywhere in it.
///
/// The two tests above go through `portfolio::read` and `Chain::mock` directly.
/// This one goes through the actor, which is where the break was: every piece
/// worked, and `Core::chain()` built a live client regardless. A test that calls
/// the mock can never notice that nothing calls the mock.
#[tokio::test(flavor = "multi_thread")]
async fn the_actor_reads_a_dashboard_from_the_scripted_chain() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (dispatcher, mut events) = chainvue_core::start(
        &tokio::runtime::Handle::current(),
        chainvue_core::Config {
            // A URL nothing may dial. If mock mode ever stops intercepting, this
            // fails as a connection error rather than quietly reading a chain.
            nodes: vec![chainvue_chain::Node::builtin(
                0,
                "Scripted chain",
                "mock://scripted",
            )],
            network: chainvue_chain::Network::Testnet,
            mock: true,
            home: dir.path().to_path_buf(),
        },
    );

    dispatcher.send(chainvue_protocol::Command::CreateWallet {
        name: "demo".to_string(),
        passphrase: chainvue_protocol::Secret::from("correct-horse-battery-staple-9931"),
    });

    // Wait for the balance to arrive rather than for a fixed time. A refresh
    // runs off the actor, and the scripted chain sleeps on every read so the
    // loading states are reachable.
    let balance = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            match events.recv().await {
                Some(chainvue_protocol::Event::Portfolio(portfolio))
                    if !portfolio.balance.spendable_sats.is_empty()
                        && portfolio.balance.spendable_sats != "0" =>
                {
                    break portfolio.balance;
                }
                Some(_) => {}
                None => panic!("the core stopped before sending a balance"),
            }
        }
    })
    .await
    .expect("a dashboard within thirty seconds");

    assert_eq!(balance.spendable_sats, "41525000000");
    assert_eq!(balance.immature_sats, "11250000000");
    assert_eq!(balance.pending_in_sats, "500000000");
    assert!(
        balance.spendable_display.contains("415.25"),
        "the balance is not formatted for a person: {}",
        balance.spendable_display,
    );
}

/// Paying a VerusID by name: the form accepts it, and refuses a revoked one.
///
/// # What this is really checking
///
/// That the name never becomes the thing that gets paid. `send::validate` is
/// offline and always refuses `demo@` — a name is not base58 — so the form is
/// only usable because the core holds an answer a node gave it, and the builder
/// is handed the **i-address** rather than the text somebody typed. If that
/// substitution ever moved into the UI, this test would still pass and the
/// security boundary would be gone; so it also asserts the note names the
/// address, which is the only part a person can check.
#[tokio::test(flavor = "multi_thread")]
async fn a_verusid_can_be_paid_by_name_and_a_revoked_one_cannot() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (dispatcher, mut events) = chainvue_core::start(
        &tokio::runtime::Handle::current(),
        chainvue_core::Config {
            nodes: vec![chainvue_chain::Node::builtin(
                0,
                "Scripted chain",
                "mock://scripted",
            )],
            network: chainvue_chain::Network::Testnet,
            mock: true,
            home: dir.path().to_path_buf(),
        },
    );

    dispatcher.send(chainvue_protocol::Command::CreateWallet {
        name: "demo".to_string(),
        passphrase: chainvue_protocol::Secret::from("correct-horse-battery-staple-9931"),
    });

    // `demo@` is in the scripted chain; `nobody@` is not.
    for (typed, want_valid, want_in_note) in [
        ("demo@", true, "iGRp1CGkuro3LtGazX8W1PRjVupPVfe8Pv"),
        ("gone@", false, "revoked"),
        ("nobody@", false, "No VerusID by that name"),
    ] {
        let verdict = resolve(&dispatcher, &mut events, typed).await;
        assert_eq!(
            verdict.to_valid, want_valid,
            "`{typed}` validity is wrong: {}",
            verdict.to_note,
        );
        assert!(
            verdict.to_note.contains(want_in_note),
            "`{typed}` says {:?}, which does not mention {want_in_note:?}",
            verdict.to_note,
        );
    }
}

/// Type `typed` into the recipient field and wait for the verdict that is no
/// longer "looking up".
///
/// The lookup is a round trip, so the first verdict back is the provisional one
/// — waiting for a settled answer is what a person does too.
async fn resolve(
    dispatcher: &chainvue_core::Dispatcher,
    events: &mut tokio::sync::mpsc::UnboundedReceiver<chainvue_protocol::Event>,
    typed: &str,
) -> chainvue_protocol::DraftValidationVm {
    dispatcher.send(chainvue_protocol::Command::ValidateDraft(
        chainvue_protocol::SendDraft {
            from_label: "main".to_string(),
            to: typed.to_string(),
            amount: "1".to_string(),
        },
    ));

    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            match events.recv().await {
                Some(chainvue_protocol::Event::SendValidation(verdict))
                    if !verdict.to_note.contains("Looking") =>
                {
                    break verdict;
                }
                Some(_) => {}
                None => panic!("the core stopped before answering about {typed}"),
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("`{typed}` never resolved"))
}

/// An identity you looked up is never counted among the ones you control.
///
/// # The failure this is here for
///
/// The screen kept one list with a `mine` flag on each row, on the theory that
/// one list was simpler than two. It was simpler and it was wrong. The heading
/// over it says the identities were *found by asking the chain which names your
/// keys control* — and a stranger's identity, looked up once, sat under that
/// sentence and was counted by it, with no way to remove it short of restarting
/// the wallet. The detail sheet said plainly that the keys were not there,
/// while the list two clicks away said the opposite.
///
/// A fact about your keys and the answer to a question you just asked are
/// different things. One list cannot be honest about both.
#[tokio::test(flavor = "multi_thread")]
async fn a_looked_up_identity_is_never_counted_as_one_of_yours() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (dispatcher, mut events) = chainvue_core::start(
        &tokio::runtime::Handle::current(),
        chainvue_core::Config {
            nodes: vec![chainvue_chain::Node::builtin(
                0,
                "Scripted chain",
                "mock://scripted",
            )],
            network: chainvue_chain::Network::Testnet,
            mock: true,
            home: dir.path().to_path_buf(),
        },
    );

    dispatcher.send(chainvue_protocol::Command::CreateWallet {
        name: "demo".to_string(),
        passphrase: chainvue_protocol::Secret::from("correct-horse-battery-staple-9931"),
    });
    dispatcher.send(chainvue_protocol::Command::RefreshIdentities);

    // The scripted chain gives this wallet three identities of its own.
    let (yours, looked_up) = identities(&mut events, |yours, _| yours.len() >= 3).await;
    assert!(looked_up.is_empty(), "nothing has been looked up yet");
    assert!(
        yours.iter().all(|row| row.mine),
        "the address-scoped search returned something this wallet cannot sign for",
    );
    let owned = yours.len();

    // Now look up somebody else's.
    dispatcher.send(chainvue_protocol::Command::LookUpIdentity(
        "stranger@".to_string(),
    ));
    let (yours, looked_up) = identities(&mut events, |_, looked| !looked.is_empty()).await;

    assert_eq!(
        yours.len(),
        owned,
        "a stranger's identity was counted among the ones your keys control",
    );
    assert!(
        yours.iter().all(|row| row.name != "stranger.VRSCTEST@"),
        "it landed in the wrong list",
    );
    assert_eq!(looked_up.len(), 1);
    assert!(!looked_up[0].mine);

    // And it can be got rid of, which is the other half of the bug: it used to
    // stay until the wallet was restarted.
    dispatcher.send(chainvue_protocol::Command::ClearLookups);
    let (yours, looked_up) = identities(&mut events, |_, looked| looked.is_empty()).await;
    assert!(looked_up.is_empty());
    assert_eq!(yours.len(), owned, "clearing lookups touched your own");
}

/// Wait for an `Identities` event whose two lists satisfy `ready`.
async fn identities(
    events: &mut tokio::sync::mpsc::UnboundedReceiver<chainvue_protocol::Event>,
    ready: impl Fn(&[chainvue_protocol::IdentityVm], &[chainvue_protocol::IdentityVm]) -> bool,
) -> (
    Vec<chainvue_protocol::IdentityVm>,
    Vec<chainvue_protocol::IdentityVm>,
) {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            match events.recv().await {
                Some(chainvue_protocol::Event::Identities { yours, looked_up })
                    if ready(&yours, &looked_up) =>
                {
                    break (yours, looked_up);
                }
                Some(_) => {}
                None => panic!("the core stopped before answering about identities"),
            }
        }
    })
    .await
    .expect("an identities event within thirty seconds")
}

/// Refreshing the watch list does not open anything.
///
/// # The failure this is here for
///
/// Watched identities are re-read on refresh, and that re-read went through the
/// same path a lookup does — the one that opens the detail sheet. So opening the
/// Identities screen after a restart flung the sheet open on whichever watched
/// row answered first, with nobody having clicked anything.
///
/// Reading something to show it and reading it to keep a row current are
/// different intentions, and only one of them is a request to look at it.
#[tokio::test(flavor = "multi_thread")]
async fn refreshing_the_watch_list_opens_no_sheet() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (dispatcher, mut events) = chainvue_core::start(
        &tokio::runtime::Handle::current(),
        chainvue_core::Config {
            nodes: vec![chainvue_chain::Node::builtin(
                0,
                "Scripted chain",
                "mock://scripted",
            )],
            network: chainvue_chain::Network::Testnet,
            mock: true,
            home: dir.path().to_path_buf(),
        },
    );

    dispatcher.send(chainvue_protocol::Command::CreateWallet {
        name: "demo".to_string(),
        passphrase: chainvue_protocol::Secret::from("correct-horse-battery-staple-9931"),
    });

    // Look a stranger up, which is a request to see it: the sheet opens.
    dispatcher.send(chainvue_protocol::Command::LookUpIdentity(
        "stranger@".to_string(),
    ));
    let opened = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            match events.recv().await {
                Some(chainvue_protocol::Event::IdentityDetail(Some(vm))) => break vm,
                Some(_) => {}
                None => panic!("the core stopped before opening the sheet"),
            }
        }
    })
    .await
    .expect("the lookup opens the sheet");
    assert_eq!(opened.name, "stranger.VRSCTEST@");

    // Now refresh, which re-reads the watched row. Nothing may open.
    dispatcher.send(chainvue_protocol::Command::RefreshIdentities);
    let (_, looked_up) = identities(&mut events, |_, looked| {
        looked.iter().any(|row| row.status != "Not read yet")
    })
    .await;
    assert_eq!(looked_up.len(), 1, "the refresh lost the watched row");

    // Drain briefly: an `IdentityDetail` arriving here is the bug.
    let stray = tokio::time::timeout(std::time::Duration::from_millis(1500), async {
        loop {
            if let Some(chainvue_protocol::Event::IdentityDetail(Some(vm))) = events.recv().await {
                return vm;
            }
        }
    })
    .await;
    assert!(
        stray.is_err(),
        "a refresh opened the detail sheet by itself",
    );
}

/// A name claim survives the process that made it.
///
/// # What this is really about
///
/// The salt tying step one to step two cannot be recovered from the chain. Lose
/// it and the commitment fee is spent on a claim that can never be completed,
/// with the name locked up until it expires. So the salt goes to disk **before**
/// anything is broadcast, and a wallet started afterwards has to find it and say
/// so — otherwise the money is gone and nothing on screen admits it.
///
/// The scripted chain refuses every broadcast, which is convenient here: it
/// produces exactly the state this is about — a claim that was built and written
/// down, with the send having gone nowhere.
#[tokio::test(flavor = "multi_thread")]
async fn a_name_claim_outlives_the_wallet_that_started_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().to_path_buf();

    let (dispatcher, mut events) = chainvue_core::start(
        &tokio::runtime::Handle::current(),
        chainvue_core::Config {
            nodes: vec![chainvue_chain::Node::builtin(
                0,
                "Scripted chain",
                "mock://scripted",
            )],
            network: chainvue_chain::Network::Testnet,
            mock: true,
            home: home.clone(),
        },
    );

    dispatcher.send(chainvue_protocol::Command::CreateWallet {
        name: "demo".to_string(),
        passphrase: chainvue_protocol::Secret::from("correct-horse-battery-staple-9931"),
    });
    // A node has to have answered before a claim can be built: a claim needs a
    // spend permit, and a permit needs a node that has said which chain it is
    // on. In the running application the shell asks at startup; here the test
    // has to.
    dispatcher.send(chainvue_protocol::Command::ProbeNodes);
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            match events.recv().await {
                Some(chainvue_protocol::Event::Network(vm)) if vm.effective.is_some() => break,
                Some(_) => {}
                None => panic!("the core stopped before a node answered"),
            }
        }
    })
    .await
    .expect("a node answers within thirty seconds");

    dispatcher.send(chainvue_protocol::Command::StartRegistration {
        name: "chainvue".to_string(),
        revocation_authority: String::new(),
        recovery_authority: String::new(),
    });

    let claim = wait_for_registration(&mut events, |claim| {
        claim.is_some_and(|vm| vm.name == "chainvue")
    })
    .await
    .expect("a claim was reported");
    assert_eq!(claim.name, "chainvue");

    // On disk, and with the salt in it. This is the assertion the module exists
    // for: it happened before any broadcast was attempted.
    //
    // Where on disk is the layout's decision, so it is asked for: a hardcoded
    // path would keep passing while reading a file nothing writes to.
    let path =
        chainvue_core::paths::Paths::new(home.clone(), &chainvue_chain::Network::Testnet, true)
            .registration();
    let text = std::fs::read_to_string(&path).expect("the claim was written down");
    assert!(text.contains("salt"), "the file carries no salt: {text}");

    // The process ends here.
    dispatcher.send(chainvue_protocol::Command::Shutdown);
    drop(dispatcher);

    // A new one, on the same directory.
    let (_resumed, mut events) = chainvue_core::start(
        &tokio::runtime::Handle::current(),
        chainvue_core::Config {
            nodes: vec![chainvue_chain::Node::builtin(
                0,
                "Scripted chain",
                "mock://scripted",
            )],
            network: chainvue_chain::Network::Testnet,
            mock: true,
            home,
        },
    );

    let resumed = wait_for_registration(&mut events, |claim| claim.is_some())
        .await
        .expect("the claim came back");
    assert_eq!(
        resumed.name, "chainvue",
        "a wallet restarted mid-registration did not pick the claim up",
    );
}

/// Wait for a `Registration` event matching `ready`.
async fn wait_for_registration(
    events: &mut tokio::sync::mpsc::UnboundedReceiver<chainvue_protocol::Event>,
    ready: impl Fn(Option<&chainvue_protocol::RegistrationVm>) -> bool,
) -> Option<chainvue_protocol::RegistrationVm> {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            match events.recv().await {
                Some(chainvue_protocol::Event::Registration(claim)) if ready(claim.as_deref()) => {
                    break claim.map(|boxed| *boxed);
                }
                Some(_) => {}
                None => panic!("the core stopped before reporting a claim"),
            }
        }
    })
    .await
    .expect("a registration event within thirty seconds")
}

/// Arriving at the Identities screen re-reads, even when the list is not empty.
///
/// # The failure this is here for
///
/// A name registered through the wallet did not appear in the list. Two causes,
/// both mine. The refresh that follows a registration runs immediately, when the
/// transaction is still unmined — and `identities_with_address` answers about
/// identity outputs at the current height, so an unmined one has none. And
/// arriving back at the screen only re-read when the list was **empty**, which
/// it was not. So the identity somebody had just watched being created was
/// missing until they pressed Refresh, which reads as a failure rather than as a
/// stale list.
#[tokio::test(flavor = "multi_thread")]
async fn arriving_at_the_screen_re_reads_a_list_that_is_already_full() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (dispatcher, mut events) = chainvue_core::start(
        &tokio::runtime::Handle::current(),
        chainvue_core::Config {
            nodes: vec![chainvue_chain::Node::builtin(
                0,
                "Scripted chain",
                "mock://scripted",
            )],
            network: chainvue_chain::Network::Testnet,
            mock: true,
            home: dir.path().to_path_buf(),
        },
    );

    dispatcher.send(chainvue_protocol::Command::CreateWallet {
        name: "demo".to_string(),
        passphrase: chainvue_protocol::Secret::from("correct-horse-battery-staple-9931"),
    });

    // Arrive once: the list fills.
    dispatcher.send(chainvue_protocol::Command::ScreenEntered(
        chainvue_protocol::ScreenId::Identities,
    ));
    let (yours, _) = identities(&mut events, |yours, _| yours.len() >= 3).await;
    assert_eq!(yours.len(), 3);

    // Leave, come back. The list is not empty, and it must be re-read anyway —
    // the chain may have gained the identity that was mid-registration when it
    // was last looked at.
    dispatcher.send(chainvue_protocol::Command::ScreenEntered(
        chainvue_protocol::ScreenId::Dashboard,
    ));
    dispatcher.send(chainvue_protocol::Command::ScreenEntered(
        chainvue_protocol::ScreenId::Identities,
    ));

    let again = tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            if let Some(chainvue_protocol::Event::Identities { yours, .. }) = events.recv().await {
                if yours.len() >= 3 {
                    break yours;
                }
            }
        }
    })
    .await
    .expect("arriving at the screen asked the chain again");
    assert_eq!(again.len(), 3);
}

/// A wallet with no keys yet asks the chain nothing and is not an error.
///
/// This is the state the demo build is in for the first few seconds of every
/// run — before anyone has typed a passphrase — and the scripted chain is built
/// during it, by the tip poll.
#[test]
fn a_wallet_with_no_addresses_still_has_a_chain_to_talk_to() {
    let chain = Chain::mock(&[]).expect("an empty script is a script");
    let (info, _latency) = chain.probe();
    assert_eq!(
        info.expect("the scripted chain answers").name,
        "VRSCTEST",
        "the demo build reports a chain the wallet is not set to",
    );
}
