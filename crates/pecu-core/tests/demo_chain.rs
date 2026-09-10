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

mod support;

use pecu_chain::Chain;
use pecu_core::portfolio;
use support::wait_for;
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

/// How many identities the scripted chain gives this wallet.
///
/// `demo@`, `vault@`, `gone@` and `maker@` — three states the Identities screen
/// needs, and one that a currency can actually be launched from. `stranger@` is
/// on the chain too and is deliberately not counted: it is somebody else's, and
/// keeping it out of this number is half of what these tests are about.
///
/// Named rather than written out at each of the five places that check it,
/// because it was `3` in all of them and adding the fourth identity broke a
/// test about screen navigation with a message about a count.
const OWNED: usize = 4;

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
    let mut nodes = pecu_chain::NodeManager::new(
        vec![pecu_chain::Node::builtin(
            0,
            "Scripted chain",
            "mock://scripted",
        )],
        pecu_chain::Network::Testnet,
    );
    let (info, latency) = chain.probe();
    let info = info.expect("the scripted chain answers");
    nodes
        .get_mut(0)
        .expect("the node just added")
        .record_success(&info, latency, &pecu_chain::Network::Testnet);
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
    let (dispatcher, mut events) = pecu_core::start(
        &tokio::runtime::Handle::current(),
        pecu_core::Config {
            // A URL nothing may dial. If mock mode ever stops intercepting, this
            // fails as a connection error rather than quietly reading a chain.
            nodes: vec![pecu_chain::Node::builtin(
                0,
                "Scripted chain",
                "mock://scripted",
            )],
            network: pecu_chain::Network::Testnet,
            mock: true,
            home: dir.path().to_path_buf(),
        },
    );

    dispatcher.send(pecu_protocol::Command::CreateWallet {
        name: "demo".to_string(),
        passphrase: pecu_protocol::Secret::from("correct-horse-battery-staple-9931"),
    });

    // Wait for the balance to arrive rather than for a fixed time. A refresh
    // runs off the actor, and the scripted chain sleeps on every read so the
    // loading states are reachable.
    let balance = wait_for(&mut events, "a balance on the dashboard", |event| match event {
        pecu_protocol::Event::Portfolio(portfolio)
            if !portfolio.balance.spendable_sats.is_empty()
                && portfolio.balance.spendable_sats != "0" =>
        {
            Some(portfolio.balance)
        }
        _ => None,
    })
    .await;

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
    let (dispatcher, mut events) = pecu_core::start(
        &tokio::runtime::Handle::current(),
        pecu_core::Config {
            nodes: vec![pecu_chain::Node::builtin(
                0,
                "Scripted chain",
                "mock://scripted",
            )],
            network: pecu_chain::Network::Testnet,
            mock: true,
            home: dir.path().to_path_buf(),
        },
    );

    dispatcher.send(pecu_protocol::Command::CreateWallet {
        name: "demo".to_string(),
        passphrase: pecu_protocol::Secret::from("correct-horse-battery-staple-9931"),
    });

    // `demo@` is in the scripted chain; `nobody@` is not.
    //
    // Asserted on the **code**, not on the sentence. The core names the reason
    // and the interface owns the words, so a test matching on prose would fail
    // the day the wording improved and pass the day the meaning changed. What
    // the note carries with it — the i-address a name resolved to — is checked
    // in the arguments, which is where the fact lives.
    for (typed, want_valid, want_code, want_arg) in [
        (
            "demo@",
            true,
            "verusid-resolved",
            Some("iGRp1CGkuro3LtGazX8W1PRjVupPVfe8Pv"),
        ),
        ("gone@", false, "verusid-revoked", None),
        ("nobody@", false, "verusid-unknown", None),
    ] {
        let verdict = resolve(&dispatcher, &mut events, typed).await;
        assert_eq!(
            verdict.to_valid, want_valid,
            "`{typed}` validity is wrong: {:?}",
            verdict.to_note,
        );
        assert_eq!(
            verdict.to_note.code, want_code,
            "`{typed}` says {:?}",
            verdict.to_note,
        );
        if let Some(want_arg) = want_arg {
            assert!(
                verdict.to_note.args.iter().any(|arg| arg == want_arg),
                "`{typed}` says {:?}, which does not carry {want_arg:?}",
                verdict.to_note,
            );
        }
    }
}

/// Type `typed` into the recipient field and wait for the verdict that is no
/// longer "looking up".
///
/// The lookup is a round trip, so the first verdict back is the provisional one
/// — waiting for a settled answer is what a person does too.
async fn resolve(
    dispatcher: &pecu_core::Dispatcher,
    events: &mut tokio::sync::mpsc::UnboundedReceiver<pecu_protocol::Event>,
    typed: &str,
) -> pecu_protocol::DraftValidationVm {
    dispatcher.send(pecu_protocol::Command::ValidateDraft(
        pecu_protocol::SendDraft {
            // The transparent route, which is what the demo chain scripts.
            from_pool: pecu_protocol::Pool::Transparent,
            from_label: "main".to_string(),
            to: typed.to_string(),
            amount: "1".to_string(),
            // A typed amount, which is what this asks about.
            send_all: false,
        },
    ));

    wait_for(events, &format!("a settled verdict on `{typed}`"), |event| match event {
        pecu_protocol::Event::SendValidation(verdict)
            if verdict.to_note.code != "verusid-resolving" =>
        {
            Some(verdict)
        }
        _ => None,
    })
    .await
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
    let (dispatcher, mut events) = pecu_core::start(
        &tokio::runtime::Handle::current(),
        pecu_core::Config {
            nodes: vec![pecu_chain::Node::builtin(
                0,
                "Scripted chain",
                "mock://scripted",
            )],
            network: pecu_chain::Network::Testnet,
            mock: true,
            home: dir.path().to_path_buf(),
        },
    );

    dispatcher.send(pecu_protocol::Command::CreateWallet {
        name: "demo".to_string(),
        passphrase: pecu_protocol::Secret::from("correct-horse-battery-staple-9931"),
    });
    dispatcher.send(pecu_protocol::Command::RefreshIdentities);

    // The scripted chain gives this wallet four identities of its own.
    let (yours, looked_up) = identities(&mut events, |yours, _| yours.len() >= OWNED).await;
    assert!(looked_up.is_empty(), "nothing has been looked up yet");
    assert!(
        yours.iter().all(|row| row.mine),
        "the address-scoped search returned something this wallet cannot sign for",
    );
    let owned = yours.len();

    // Now look up somebody else's.
    dispatcher.send(pecu_protocol::Command::LookUpIdentity(
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
    dispatcher.send(pecu_protocol::Command::ClearLookups);
    let (yours, looked_up) = identities(&mut events, |_, looked| looked.is_empty()).await;
    assert!(looked_up.is_empty());
    assert_eq!(yours.len(), owned, "clearing lookups touched your own");
}

/// Wait for an `Identities` event whose two lists satisfy `ready`.
async fn identities(
    events: &mut tokio::sync::mpsc::UnboundedReceiver<pecu_protocol::Event>,
    ready: impl Fn(&[pecu_protocol::IdentityVm], &[pecu_protocol::IdentityVm]) -> bool,
) -> (
    Vec<pecu_protocol::IdentityVm>,
    Vec<pecu_protocol::IdentityVm>,
) {
    wait_for(events, "identities the caller recognises", |event| match event {
        pecu_protocol::Event::Identities { yours, looked_up } if ready(&yours, &looked_up) => {
            Some((yours, looked_up))
        }
        _ => None,
    })
    .await
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
    let (dispatcher, mut events) = pecu_core::start(
        &tokio::runtime::Handle::current(),
        pecu_core::Config {
            nodes: vec![pecu_chain::Node::builtin(
                0,
                "Scripted chain",
                "mock://scripted",
            )],
            network: pecu_chain::Network::Testnet,
            mock: true,
            home: dir.path().to_path_buf(),
        },
    );

    dispatcher.send(pecu_protocol::Command::CreateWallet {
        name: "demo".to_string(),
        passphrase: pecu_protocol::Secret::from("correct-horse-battery-staple-9931"),
    });

    // Look a stranger up, which is a request to see it: the sheet opens.
    dispatcher.send(pecu_protocol::Command::LookUpIdentity(
        "stranger@".to_string(),
    ));
    let opened = wait_for(&mut events, "the sheet a lookup opens", |event| match event {
        pecu_protocol::Event::IdentityDetail(Some(vm)) => Some(vm),
        _ => None,
    })
    .await;
    assert_eq!(opened.name, "stranger.VRSCTEST@");

    // Now refresh, which re-reads the watched row. Nothing may open.
    dispatcher.send(pecu_protocol::Command::RefreshIdentities);
    let (_, looked_up) = identities(&mut events, |_, looked| {
        looked.iter().any(|row| row.status != "Not read yet")
    })
    .await;
    assert_eq!(looked_up.len(), 1, "the refresh lost the watched row");

    // Drain briefly: an `IdentityDetail` arriving here is the bug.
    //
    // A clock, and one of the two #47 deliberately left as clocks. This waits
    // for something to *not* happen, so there is no state to wait on and no
    // predicate that could end it early. A loaded machine makes this assertion
    // weaker rather than red — the worst it can do is fail to see a sheet that
    // was opened — which is the opposite of the failure the rest of this file's
    // budgets had.
    let stray = tokio::time::timeout(std::time::Duration::from_millis(1500), async {
        loop {
            if let Some(pecu_protocol::Event::IdentityDetail(Some(vm))) = events.recv().await {
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

    let (dispatcher, mut events) = pecu_core::start(
        &tokio::runtime::Handle::current(),
        pecu_core::Config {
            nodes: vec![pecu_chain::Node::builtin(
                0,
                "Scripted chain",
                "mock://scripted",
            )],
            network: pecu_chain::Network::Testnet,
            mock: true,
            home: home.clone(),
        },
    );

    dispatcher.send(pecu_protocol::Command::CreateWallet {
        name: "demo".to_string(),
        passphrase: pecu_protocol::Secret::from("correct-horse-battery-staple-9931"),
    });
    // A node has to have answered before a claim can be built: a claim needs a
    // spend permit, and a permit needs a node that has said which chain it is
    // on. In the running application the shell asks at startup; here the test
    // has to.
    dispatcher.send(pecu_protocol::Command::ProbeNodes);
    wait_for(&mut events, "a node that says which chain it is on", |event| match event {
        pecu_protocol::Event::Network(vm) if vm.effective.is_some() => Some(()),
        _ => None,
    })
    .await;

    dispatcher.send(pecu_protocol::Command::StartRegistration {
        name: "pecu".to_string(),
        revocation_authority: String::new(),
        recovery_authority: String::new(),
    });

    let claim = wait_for_registration(&mut events, |claim| {
        claim.is_some_and(|vm| vm.name == "pecu")
    })
    .await
    .expect("a claim was reported");
    assert_eq!(claim.name, "pecu");

    // On disk, and with the salt in it. This is the assertion the module exists
    // for: it happened before any broadcast was attempted.
    //
    // Where on disk is the layout's decision, so it is asked for: a hardcoded
    // path would keep passing while reading a file nothing writes to.
    let path =
        pecu_core::paths::Paths::new(home.clone(), &pecu_chain::Network::Testnet, true)
            .registration();
    let text = std::fs::read_to_string(&path).expect("the claim was written down");
    assert!(text.contains("salt"), "the file carries no salt: {text}");

    // The process ends here.
    dispatcher.send(pecu_protocol::Command::Shutdown);
    drop(dispatcher);

    // A new one, on the same directory.
    let (_resumed, mut events) = pecu_core::start(
        &tokio::runtime::Handle::current(),
        pecu_core::Config {
            nodes: vec![pecu_chain::Node::builtin(
                0,
                "Scripted chain",
                "mock://scripted",
            )],
            network: pecu_chain::Network::Testnet,
            mock: true,
            home,
        },
    );

    let resumed = wait_for_registration(&mut events, |claim| claim.is_some())
        .await
        .expect("the claim came back");
    assert_eq!(
        resumed.name, "pecu",
        "a wallet restarted mid-registration did not pick the claim up",
    );
}

/// Wait for a `Registration` event matching `ready`.
async fn wait_for_registration(
    events: &mut tokio::sync::mpsc::UnboundedReceiver<pecu_protocol::Event>,
    ready: impl Fn(Option<&pecu_protocol::RegistrationVm>) -> bool,
) -> Option<pecu_protocol::RegistrationVm> {
    wait_for(events, "a claim the caller recognises", |event| match event {
        pecu_protocol::Event::Registration(claim) if ready(claim.as_deref()) => {
            Some(claim.map(|boxed| *boxed))
        }
        _ => None,
    })
    .await
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
    let (dispatcher, mut events) = pecu_core::start(
        &tokio::runtime::Handle::current(),
        pecu_core::Config {
            nodes: vec![pecu_chain::Node::builtin(
                0,
                "Scripted chain",
                "mock://scripted",
            )],
            network: pecu_chain::Network::Testnet,
            mock: true,
            home: dir.path().to_path_buf(),
        },
    );

    dispatcher.send(pecu_protocol::Command::CreateWallet {
        name: "demo".to_string(),
        passphrase: pecu_protocol::Secret::from("correct-horse-battery-staple-9931"),
    });

    // Arrive once: the list fills.
    dispatcher.send(pecu_protocol::Command::ScreenEntered(
        pecu_protocol::ScreenId::Identities,
    ));
    let (yours, _) = identities(&mut events, |yours, _| yours.len() >= OWNED).await;
    assert_eq!(yours.len(), OWNED);

    // Leave, come back. The list is not empty, and it must be re-read anyway —
    // the chain may have gained the identity that was mid-registration when it
    // was last looked at.
    dispatcher.send(pecu_protocol::Command::ScreenEntered(
        pecu_protocol::ScreenId::Dashboard,
    ));
    dispatcher.send(pecu_protocol::Command::ScreenEntered(
        pecu_protocol::ScreenId::Identities,
    ));

    let (again, _) = identities(&mut events, |yours, _| yours.len() >= OWNED).await;
    assert_eq!(again.len(), OWNED);
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

/// Opening the markets screen fills it, from the same path the real chain uses.
///
/// # Why this test exists, in the same words as the one at the top of this file
///
/// The markets screen shipped as a *drawing*. Every figure on it was a fixture
/// in the interface crate, the bridge wrote nothing into `MarketState`, and no
/// command existed that would have filled it — so the screen was blank in every
/// running build, mock or live, and looked complete in every reference image.
/// Nothing caught it, because the only thing that had ever rendered that screen
/// was the fixture that drew it.
///
/// So this asserts the number rather than the shape: one VRSCTEST is worth
/// `0.5372` DAI.vETH, which is what the reserve state the scripted chain
/// publishes divides out to, and what the daemon's own `estimateconversion`
/// answered on that state to within its conversion fee.
#[tokio::test(flavor = "multi_thread")]
#[allow(clippy::too_many_lines)]
async fn opening_the_markets_screen_prices_the_chain_currency() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (dispatcher, mut events) = pecu_core::start(
        &tokio::runtime::Handle::current(),
        pecu_core::Config {
            nodes: vec![pecu_chain::Node::builtin(
                0,
                "Scripted chain",
                "mock://scripted",
            )],
            network: pecu_chain::Network::Testnet,
            mock: true,
            home: dir.path().to_path_buf(),
        },
    );

    dispatcher.send(pecu_protocol::Command::CreateWallet {
        name: "demo".to_string(),
        passphrase: pecu_protocol::Secret::from("correct-horse-battery-staple-9931"),
    });
    dispatcher.send(pecu_protocol::Command::ScreenEntered(
        pecu_protocol::ScreenId::Markets,
    ));

    let (rows, quote) = wait_for(&mut events, "a filled markets screen", |event| match event {
        pecu_protocol::Event::Markets { rows, quote } if !rows.is_empty() => Some((rows, quote)),
        _ => None,
    })
    .await;

    // The column has to be able to say what it is denominated in, or every
    // figure under it is a ratio of nothing.
    assert_eq!(quote, "DAI.vETH");

    let vrsctest = rows
        .iter()
        .find(|row| row.name == "VRSCTEST")
        .expect("the chain's own currency is on its own markets screen");
    assert_eq!(vrsctest.price, "0.5372", "{vrsctest:?}");

    // The quote currency is one of itself, and every row is honest about a
    // change nothing on this chain can answer for yet.
    let dai = rows
        .iter()
        .find(|row| row.name == "DAI.vETH")
        .expect("the currency everything is priced in");
    assert_eq!(dai.price, "1.00");
    // Bridge.vETH published one state in thirty days, so everything priced
    // through it did not move — which is a figure rather than an absence.
    assert_eq!(vrsctest.change, "0.00%", "{rows:?}");
    assert_eq!(vrsctest.tone, "unknown");

    // And the one pool that did move says so, with a sign and a colour.
    let moved = rows
        .iter()
        .find(|row| row.name == "vrealv1")
        .expect("the pool with a history is a row");
    assert!(moved.change.starts_with('+'), "{moved:?}");
    assert_eq!(moved.tone, "positive");

    // A currency no started basket trades is not on the table at all.
    //
    // `Bridge.Betelgeuse` is on the scripted chain and in the book — it is a
    // real currency — and nothing started holds it, so no single conversion can
    // reach it. It used to sit at the bottom as a row of four dashes, which is
    // an offer to convert something that cannot be converted. This list is also
    // where the convert picker gets its rows.
    assert!(
        !rows.iter().any(|row| row.name == "Bridge.Betelgeuse"),
        "a currency nothing can convert has no row: {rows:?}",
    );
    assert!(
        rows.iter().all(|row| row.price != "—"),
        "every row left is one a route can price: {rows:?}",
    );

    // And the detail, which must come out of the same book rather than a second
    // read that could quote a different block.
    dispatcher.send(pecu_protocol::Command::OpenMarket(vrsctest.address.clone()));
    let detail = wait_for(&mut events, "the detail of the row that was opened", |event| {
        match event {
            pecu_protocol::Event::MarketDetail(Some(detail)) => Some(detail),
            _ => None,
        }
    })
    .await;

    assert_eq!(detail.name, "VRSCTEST");
    assert_eq!(detail.price, "0.5372");
    assert_eq!(detail.route, "VRSCTEST  →  Bridge.vETH  →  DAI.vETH");

    // Every pool that trades it is listed; only the started ones set a price.
    let states: Vec<&str> = detail
        .venues
        .iter()
        .map(|venue| venue.state.as_str())
        .collect();
    assert_eq!(
        states,
        vec!["active", "active", "unstarted"],
        "{:?}",
        detail.venues
    );

    // And the history, which is the whole reason `getcurrencystate` gained a
    // range. Bridge.vETH published one reading in thirty days on VRSCTEST, so
    // the chain currency's line is flat — a real picture of an idle market
    // rather than an empty one. The change column says `—` because nothing
    // moved to measure, which is not the same as "it moved by zero".
    assert!(detail.series.len() > 20, "{:?}", detail.series.len());
    let flat = detail.series[0].sats;
    assert!(detail.series.iter().all(|p| p.sats == flat));
    // Spread across the window, not stacked at one instant.
    assert!(
        detail.series.last().expect("points").t - detail.series[0].t > 20 * 86_400,
        "the samples are not spread over the window",
    );
    // Flat, and it says so as a figure rather than as an absence: the price is
    // known at both ends of the window and it is the same price.
    assert_eq!(detail.change, "0.00%");
    assert_eq!(detail.tone, "unknown", "flat is neither a gain nor a loss");

    dispatcher.send(pecu_protocol::Command::OpenMarket(
        "iBBRjDbPf3wdFpghLotJQ3ESjtPBxn6NS3".to_string(),
    ));
    let moving = wait_for(&mut events, "the detail of a market with a history", |event| {
        match event {
            pecu_protocol::Event::MarketDetail(Some(detail)) if detail.name == "vrealv1" => {
                Some(detail)
            }
            _ => None,
        }
    })
    .await;

    assert!(moving.series.len() > 20, "{}", moving.series.len());
    // Oldest first, rising, and a day apart — the supply falls and the holding
    // does not.
    assert!(
        moving.series.windows(2).all(|w| w[1].sats >= w[0].sats),
        "{:?}",
        moving.series,
    );
    // Six supply steps reach the price, and no more: the readings between them
    // repeat, which is what a chain does between notarizations.
    let steps: std::collections::BTreeSet<i64> =
        moving.series.iter().map(|p| p.sats).collect();
    assert_eq!(steps.len(), 6, "{steps:?}");

    // A change with a sign and a colour, over the window the chart draws —
    // which is what nothing in this wallet could say before the SDK could ask
    // for a past height.
    assert!(moving.change.starts_with('+'), "{}", moving.change);
    assert_eq!(moving.tone, "positive");
}

/// The dashboard gets prices without anybody opening the markets screen.
///
/// The dashboard carries a markets column beside the balance, and the read that
/// fills it is scoped to a screen — so it would be entirely possible to ship a
/// wallet whose markets screen worked and whose dashboard column was permanently
/// empty, because nothing on the way to the dashboard asks. Creating a wallet is
/// the only thing this test does.
#[tokio::test(flavor = "multi_thread")]
async fn the_dashboard_gets_prices_without_visiting_the_markets_screen() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (dispatcher, mut events) = pecu_core::start(
        &tokio::runtime::Handle::current(),
        pecu_core::Config {
            nodes: vec![pecu_chain::Node::builtin(
                0,
                "Scripted chain",
                "mock://scripted",
            )],
            network: pecu_chain::Network::Testnet,
            mock: true,
            home: dir.path().to_path_buf(),
        },
    );

    dispatcher.send(pecu_protocol::Command::CreateWallet {
        name: "demo".to_string(),
        passphrase: pecu_protocol::Secret::from("correct-horse-battery-staple-9931"),
    });

    let rows = wait_for(&mut events, "prices nobody navigated to a screen for", |event| {
        match event {
            pecu_protocol::Event::Markets { rows, .. } if !rows.is_empty() => Some(rows),
            _ => None,
        }
    })
    .await;

    assert!(
        rows.iter().any(|row| row.name == "VRSCTEST"),
        "{rows:?}",
    );
}

/// The dashboard's values and the markets table come out of one price book.
///
/// # The failure this exists to prevent
///
/// Issue #14 asks for a value on every holding, and the whole of the risk in it
/// is that there are now two screens quoting one book. Somebody who wants to
/// check the ASSETS column divides: their VRSCTEST holding is 527.75 and the row
/// says it is worth 283.52, so the wallet is quoting 0.5372 DAI.vETH to the
/// coin — which is a figure the markets table prints two columns away. If the
/// dashboard were holding values from the book before last, that division would
/// not come out, and nothing on either screen would say which of the two was
/// behind.
///
/// Two things are asserted, and neither is the formatting.
///
/// **The figures agree.** Not by recomputing the price here, which would only
/// test that this file can multiply: the implied price is taken out of the
/// dashboard — value divided by amount — and compared against the string the
/// markets table is showing at the same moment, to the precision that table
/// prints.
///
/// **A markets read republishes the dashboard.** This is the part that cannot
/// be seen from one event. The book is replaced on a schedule of its own, with
/// nothing about the balances changing, so the values on screen have to move
/// with it — `Core::emit_markets` publishes both views in one call for exactly
/// this reason. The second phase takes a wallet that has settled, asks for a
/// markets read alone, and waits for the dashboard. Without that one line the
/// portfolio never arrives and this times out.
#[tokio::test(flavor = "multi_thread")]
async fn the_dashboard_values_what_it_holds_from_the_book_the_markets_table_shows() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (dispatcher, mut events) = pecu_core::start(
        &tokio::runtime::Handle::current(),
        pecu_core::Config {
            nodes: vec![pecu_chain::Node::builtin(
                0,
                "Scripted chain",
                "mock://scripted",
            )],
            network: pecu_chain::Network::Testnet,
            mock: true,
            home: dir.path().to_path_buf(),
        },
    );

    dispatcher.send(pecu_protocol::Command::CreateWallet {
        name: "demo".to_string(),
        passphrase: pecu_protocol::Secret::from("correct-horse-battery-staple-9931"),
    });

    // Both views, as the wallet settles. Whichever of the two reads finishes
    // first, a priced dashboard only exists once both have — which is the
    // coupling, seen from the outside.
    let (priced, rows) = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        let mut portfolio: Option<pecu_protocol::PortfolioVm> = None;
        let mut rows: Vec<pecu_protocol::MarketRowVm> = Vec::new();
        loop {
            match events.recv().await {
                Some(pecu_protocol::Event::Portfolio(vm)) => portfolio = Some(vm),
                Some(pecu_protocol::Event::Markets { rows: fresh, .. }) if !fresh.is_empty() => {
                    rows = fresh;
                }
                Some(_) => {}
                None => panic!("the core stopped before pricing the dashboard"),
            }
            let ready = portfolio
                .as_ref()
                .and_then(|vm| vm.assets.iter().find(|asset| asset.native))
                .is_some_and(|native| native.value_sats.is_some());
            if ready && !rows.is_empty() {
                break (portfolio.expect("checked just above"), rows);
            }
        }
    })
    .await
    .expect("a dashboard with a value on it, and the table it has to agree with");

    let native = priced
        .assets
        .iter()
        .find(|asset| asset.native)
        .expect("the chain's own currency has a row");
    let row = rows
        .iter()
        .find(|row| row.name == "VRSCTEST")
        .expect("the chain's own currency has a market row");

    assert_eq!(
        implied_price(native),
        row.price,
        "the dashboard values {} {} at {} — which is not the price the markets \
         table is showing for it, so the two screens are quoting different books",
        native.amount_display,
        native.name,
        native.value_display,
    );
    // And the figure itself, so that a test which agreed with a table that had
    // gone wrong would still fail. 527.75 coins at 0.5372 DAI.vETH.
    assert_eq!(native.value_display, "283.52", "{native:?}");

    // ── A book that moves takes the dashboard with it ────────────────────
    //
    // Nothing is asked about the balances here. A markets read alone has to
    // produce a dashboard, or the values on screen outlive the prices they were
    // taken from.
    dispatcher.send(pecu_protocol::Command::RefreshMarkets);
    let again = tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            match events.recv().await {
                Some(pecu_protocol::Event::Portfolio(vm)) => {
                    if vm.assets.iter().any(|asset| asset.native) {
                        break vm;
                    }
                }
                Some(_) => {}
                None => panic!("the core stopped before republishing the dashboard"),
            }
        }
    })
    .await
    .expect("a markets read republishes the holdings it has just repriced");

    let native = again
        .assets
        .iter()
        .find(|asset| asset.native)
        .expect("the chain's own currency has a row");
    assert_eq!(
        implied_price(native),
        row.price,
        "the repriced dashboard disagrees with the table: {native:?}",
    );
}

/// What the dashboard is quoting, read back off it the way a person would.
///
/// Value divided by amount, as a price, in the words the markets table uses —
/// so the comparison is between two strings a person can see on two screens
/// rather than between a figure and a recomputation of itself.
///
/// Integers until the last step, because that is the rule the value path keeps:
/// the division is taken in satoshis and only the ratio becomes a float, for
/// the formatter that demands one. Nothing here is money.
#[allow(clippy::cast_precision_loss)]
fn implied_price(asset: &pecu_protocol::AssetVm) -> String {
    let value: i128 = asset
        .value_sats
        .as_ref()
        .expect("a priced row")
        .parse()
        .expect("satoshis, as decimal");
    let held: i128 = asset.amount_sats.parse().expect("satoshis, as decimal");
    let per_coin = i128::from(pecu_protocol::SATS_PER_COIN);
    let sats = (value * per_coin + held / 2) / held;
    pecu_protocol::format::price(sats as f64 / per_coin as f64)
}

/// A conversion is priced end to end, by the node rather than by the wallet.
///
/// The distinction this asserts is the one the convert screen exists to make.
/// `market::Book` divides two reserve figures and gets a **mid** price —
/// 0.5372 DAI to the VRSCTEST, what the currency is worth. `estimateconversion`
/// answers what *this* conversion at *this* size yields after the pool has been
/// moved by it and the fee taken: 0.5341. The gap between them is the slippage
/// line, and a wallet quoting the first would be quoting a price it cannot
/// honour.
#[tokio::test(flavor = "multi_thread")]
async fn a_conversion_is_priced_by_the_node_and_not_by_the_mid_price() {
    const VRSCTEST: &str = "iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq";
    const DAI: &str = "iN9vbHXexEh6GTZ45fRoJGKTQThfbgUwMh";

    let dir = tempfile::tempdir().expect("tempdir");
    let (dispatcher, mut events) = pecu_core::start(
        &tokio::runtime::Handle::current(),
        pecu_core::Config {
            nodes: vec![pecu_chain::Node::builtin(
                0,
                "Scripted chain",
                "mock://scripted",
            )],
            network: pecu_chain::Network::Testnet,
            mock: true,
            home: dir.path().to_path_buf(),
        },
    );

    dispatcher.send(pecu_protocol::Command::CreateWallet {
        name: "demo".to_string(),
        passphrase: pecu_protocol::Secret::from("correct-horse-battery-staple-9931"),
    });

    // The balance has to be in before an amount can be checked against it.
    wait_for(&mut events, "a balance with assets in it", |event| match event {
        pecu_protocol::Event::Portfolio(portfolio) if !portfolio.assets.is_empty() => Some(()),
        _ => None,
    })
    .await;

    let quote = quote_for(&dispatcher, &mut events, VRSCTEST, DAI, "250").await;

    assert!(quote.ready, "{quote:?}");
    assert_eq!(quote.get, "133.5245 8143");
    assert_eq!(quote.via, "Bridge.vETH");
    assert_eq!(quote.rate, "1 VRSCTEST = 0.5341 DAI.vETH");
    assert_eq!(quote.slippage, "0.58%");
    assert_eq!(quote.slippage_tone, "positive");
    // Two currencies, and each figure says which one it is in.
    assert_eq!(quote.conversion_fee, "0.1250 0000 VRSCTEST");
    assert!(quote.minimum.ends_with(" DAI.vETH"), "{}", quote.minimum);
    assert_eq!(quote.note.code, "");

    // More than the wallet holds is refused here, without a node being asked.
    let refused = quote_for(&dispatcher, &mut events, VRSCTEST, DAI, "9000").await;
    assert!(!refused.ready);
    assert_eq!(refused.note.code, "convert-above-holding");

    // And a currency no started basket holds is refused for a different
    // reason — one that is about the chain rather than about this wallet.
    let unroutable = quote_for(
        &dispatcher,
        &mut events,
        VRSCTEST,
        "iGRp1CGkuro3LtGazX8W1PRjVupPVfe8Pv",
        "250",
    )
    .await;
    assert!(!unroutable.ready);
    assert_eq!(unroutable.note.code, "convert-no-route");
}

/// Send a draft and wait for the quote that answers it.
async fn quote_for(
    dispatcher: &pecu_core::Dispatcher,
    events: &mut tokio::sync::mpsc::UnboundedReceiver<pecu_protocol::Event>,
    from: &str,
    to: &str,
    pay: &str,
) -> pecu_protocol::ConvertQuoteVm {
    dispatcher.send(pecu_protocol::Command::SetConvertDraft(
        pecu_protocol::ConvertDraft {
            from: from.to_string(),
            to: to.to_string(),
            pay: pay.to_string(),
        },
    ));

    wait_for(events, &format!("a quote for {pay}"), |event| match event {
        pecu_protocol::Event::ConvertQuote(quote) => Some(*quote),
        _ => None,
    })
    .await
}
