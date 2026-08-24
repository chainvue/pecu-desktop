//! The five ways this wallet changes an identity, built and signed against a
//! chain that cannot relay anything.
//!
//! # What had no test at all before this file
//!
//! The build-and-sign half of `SetIdentityAuthorities`, `LockIdentity`,
//! `UnlockIdentity`, `RevokeIdentity` and `RecoverIdentity` — which is to say
//! `identity::prepare` and the five `Change` values the commands turn into. Not
//! one of them was signed by anything, offline or live. `identity.rs`'s unit
//! tests cover the wording a change gets (`describe`), the gate it needs
//! (`needs_typed_confirmation`), the status a record derives and the
//! `IdentityChange` an authorities move turns into — all of which stop short of
//! the signature. The scripted chain covers *reading*
//! the states these operations produce: it seeds an identity that is locked,
//! one that is unlocking and one that is revoked. Nothing in between had ever
//! run, and `docs/LATER.md` §0's "covered by the scripted chain" was generous
//! about exactly this gap. §0b records what remains after it: the same five
//! against a real chain, which is `live_identity.rs` and a funded key.
//!
//! Two things on that path are still exercised by nothing, and §0b is the
//! entry somebody will read before deciding this gate is closed, so they are
//! named rather than left to be inferred: the `Command` → `Change` dispatch in
//! `lib.rs`, and `ConfirmIdentityChange`'s typed-word gate. This file starts at
//! `identity::prepare` and never puts a command on the actor's queue.
//!
//! What each test asserts is the wallet's own dispatch, not the SDK's. The
//! interesting decision is which of four SDK entry points a described change
//! belongs to — an unlock is not an update, a revocation is signed by a
//! different authority than the identity's own keys — and `identity::prepare`
//! is where that choice is made. So the assertions are on the variant of
//! `Prepared` that comes back and on the outcome hanging off it, rather than on
//! the bytes, which are the SDK's own business and have their own tests there.
//!
//! # Zero broadcasts, measured rather than argued
//!
//! `identity::prepare` cannot send, and the reason is the permit rather than
//! the reader: `Chain` does hand out a broadcaster, but only in exchange for a
//! `SpendPermit`, and the only thing that issues one is a `NodeManager` this
//! signature does not take. That is a property of the types and it is worth
//! asserting anyway, because the interesting failure is not "the flow sent
//! something" — it is a future refactor that hands the preparation step a
//! broadcaster for some unrelated convenience. `MockChain` counts attempts, and
//! the count stays at zero until one test asks for one on purpose.
//!
//! # The trap this file had to work around
//!
//! Every identity the demo script seeds is its own revocation *and* recovery
//! authority, which is the shape a registration lands in — and the shape a
//! revocation is refused for, by `verus-tx-identity` before a signature exists
//! and by consensus after. So a revocation could not be prepared against any of
//! them, and neither could the recovery that undoes it. The subjects here are
//! seeded by the test through `MockChain::seed_changeable_identity`, which
//! leaves `MockChain::demo` alone: the demo build's identity list is counted in
//! five places in `demo_chain.rs` and has broken a navigation test before by
//! moving.

// `panic!` is how a variant assertion is written here: `let Prepared::Revoked(..)
// = ... else { panic! }` says which flow was expected and prints which was
// taken, where a `matches!` assertion would only say "false".
#![cfg(feature = "mock")]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use pecu_core::identity::{self, Change, Prepared};
use pecu_keystore::{NewKey, Vault};
use pecu_mock::RecoveryAuthority;
use pecu_protocol::{NoteVm, Secret};
use verus_sdk::identity::FLAG_LOCKED;
use verus_sdk::network::ChainReader;
use verus_sdk::verus_keys::PrivateKey;

/// From the SDK's own fixtures, so the address is a known quantity.
const WIF: &str = "UusoQWsobQKUkezgBJa22D9G4t9Avo6k8wD5UUxmmfAEoTN8bawc";
const LABEL: &str = "funding";

/// How long `sealed@` is held for. Any delay does; a round number reads back
/// recognisably in a failure message.
const DELAY: u32 = 100;

struct Wallet {
    vault: Vault,
    chain: pecu_chain::Chain,
    mock: pecu_mock::MockChain,
    nodes: pecu_chain::NodeManager,
    /// The temporary directory the vault lives in. Held so it outlives the
    /// vault rather than being dropped at the end of the setup function.
    _dir: tempfile::TempDir,
}

impl Wallet {
    /// The i-address of a scripted identity, which is what anything
    /// destructive should be naming.
    ///
    /// `current_identity` can check an i-address against the object it decodes
    /// with no node involved; a `name@` lookup can only be checked against what
    /// the node itself said. Every subject here is named the strong way.
    fn address_of(&self, typed: &str) -> String {
        self.chain
            .identity(typed)
            .expect("the scripted chain knows the identity it seeded")
            .identity_address
    }
}

/// A wallet holding the key the scripted chain's identities are controlled by,
/// pointed at that chain, with three more identities seeded on top of the demo.
///
/// The three exist because the demo's four are the states the *Identities
/// screen* needs and none of them is a state a *change* can be built from:
///
/// * `ward@` — active, and its recovery authority is `demo@` rather than
///   itself. That is the only shape a revocation can legally take, and the
///   whole reason this seeding exists.
/// * `sealed@` — locked with a delay, which is the one state an unlock has
///   anything to start.
/// * `strayed@` — already revoked, because a recovery needs something to act
///   on and the scripted chain refuses the broadcast that would produce it.
fn wallet() -> Wallet {
    let dir = tempfile::tempdir().expect("tempdir");
    let passphrase = Secret::new("correct horse battery staple".to_string());
    let vault = Vault::create(&dir.path().join("vault.json"), "test", &passphrase)
        .expect("a fresh vault is created");
    vault
        .unlock(&passphrase)
        .expect("the vault just made opens");

    let key = PrivateKey::from_wif(WIF).expect("the fixture WIF is valid");
    let address = key.address().to_string();
    vault
        .add_key(LABEL, NewKey::FromWif { key })
        .expect("the key is added");

    let mock = pecu_mock::MockChain::demo(std::slice::from_ref(&address))
        .expect("the demo script builds");

    // Somebody else's identity, in the sense that matters: a different
    // identity, whose primary addresses this wallet's key is nonetheless
    // among — which is what makes the recovery signable here and what the
    // operator has to provision by hand for `live_identity.rs`.
    let guardian = mock
        .identity("demo@")
        .expect("the demo script seeds demo@")
        .identity_address;
    let elsewhere = RecoveryAuthority::Another(guardian);

    for (name, flags, unlock_after, recovery) in [
        ("ward", 0, 0, &elsewhere),
        ("sealed", FLAG_LOCKED, DELAY, &RecoveryAuthority::ItsOwn),
        ("strayed", verus_sdk::identity::FLAG_REVOKED, 0, &elsewhere),
    ] {
        mock.seed_changeable_identity(name, &address, flags, unlock_after, recovery)
            .expect("the identity is seeded");
    }

    let chain = pecu_chain::Chain::Mock(mock.clone());

    // A permit needs a node that has answered, and the answer has to name the
    // chain the wallet is set to. Same shape `currency_launch.rs` uses.
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

    Wallet {
        vault,
        chain,
        mock,
        nodes,
        _dir: dir,
    }
}

/// Build and sign one change against the scripted chain, and insist that
/// nothing left the process.
///
/// The counter is checked here rather than in each test because it is the same
/// claim every time and a test that forgot it would still pass.
fn prepared(wallet: &Wallet, subject: &str, change: &Change) -> Prepared {
    let built = identity::prepare(
        &wallet.chain,
        &wallet.vault,
        LABEL,
        &wallet.address_of(subject),
        change,
    )
    .unwrap_or_else(|reason| panic!("{change:?} would not build: {reason}"));

    assert_eq!(
        wallet.mock.broadcast_attempts(),
        0,
        "preparing {change:?} reached the network",
    );
    // Every one of these pays a miner fee out of the funding key, and the
    // review shows it. A zero here would mean the review is showing a figure
    // nobody computed.
    assert!(
        built.fee().to_sat() > 0,
        "{change:?} was signed with no fee to show on the review",
    );
    built
}

/// Pointing revocation and recovery somewhere else — the change that moves who
/// owns the identity, and the only one the SDK gates behind an explicit opt-in.
///
/// `as_sdk_change` sets that opt-in itself, and this is what proves it: without
/// `allowing_authority_change` the builder refuses the update rather than
/// producing one, so the assertion that a signature exists at all is the
/// assertion that the flag was set.
#[test]
fn an_authorities_change_is_signed_and_reports_that_it_moves_control() {
    let wallet = wallet();
    let guardian = wallet.address_of("demo@");
    let change = Change::Authorities {
        revocation: Some(guardian.clone()),
        recovery: Some(guardian.clone()),
    };

    assert_eq!(
        change.describe(),
        NoteVm::with("change-both-authorities", [guardian.clone(), guardian]),
        "the review would not name both authorities it is about to move",
    );
    assert!(change.changes_authority());
    assert!(
        !change.needs_typed_confirmation(),
        "an authorities change asks for a typed word it was never meant to",
    );

    let Prepared::Updated(unsent) = prepared(&wallet, "ward@", &change) else {
        panic!("an authorities change did not go through the update flow");
    };
    assert!(
        unsent.outcome.changes_authority,
        "the transaction moves both authorities and does not say so",
    );
}

/// Holding the funds. A lock is an ordinary update carrying a timelock, which
/// is why it is the arm that falls through to `as_sdk_change` rather than
/// having a flow of its own.
#[test]
fn a_lock_is_signed_as_an_update_and_does_not_move_control() {
    let wallet = wallet();
    let change = Change::Lock { delay: DELAY };

    assert_eq!(
        change.describe(),
        NoteVm::with("change-lock", [DELAY.to_string()]),
    );
    assert!(!change.changes_authority());

    let Prepared::Updated(unsent) = prepared(&wallet, "ward@", &change) else {
        panic!("a lock did not go through the update flow");
    };
    assert!(
        !unsent.outcome.changes_authority,
        "locking an identity reported that it moved who controls it",
    );
}

/// Starting the countdown, which is not an `IdentityChange` at all.
///
/// This is the arm most easily written wrong. Consensus measures the published
/// unlock height from the transaction's own expiry rather than from the tip, so
/// the height cannot be computed by a caller who has not yet built the
/// transaction — which is why the SDK gives unlocking its own entry point and
/// why `as_sdk_change` refuses to translate it. Sending it through the update
/// flow instead would produce a transaction the daemon rejects after the fee,
/// with a message about script verification and nothing about timelocks.
#[test]
fn starting_an_unlock_takes_the_flow_that_reads_the_delay_from_the_chain() {
    let wallet = wallet();
    let change = Change::Unlock { extra_blocks: 0 };

    assert_eq!(change.describe(), identity::unlock_note());
    assert!(
        identity::as_sdk_change(&change).is_err(),
        "an unlock translated into an IdentityChange, which cannot carry it",
    );

    let Prepared::Updated(unsent) = prepared(&wallet, "sealed@", &change) else {
        panic!("an unlock did not go through the unlock flow");
    };
    assert!(!unsent.outcome.changes_authority);
}

/// The one that cannot be taken back, on the one identity in the script that
/// can legally take it.
///
/// What is pinned is the `Prepared::Revoked` variant — that a revocation takes
/// the revocation flow rather than falling through to the update one — and
/// that the transaction names the authority the chain holds.
///
/// It cannot pin more than that. Every identity this script seeds keeps its
/// *revocation* authority pointed at itself, which is the authority a wallet
/// holding the primary keys can actually satisfy, so the address asserted below
/// is the subject's own and a delegated revocation authority would look
/// identical from here. The recovery test is the one that genuinely
/// distinguishes: its authority is `demo@`, a different identity.
#[test]
fn a_revocation_is_signed_against_the_revocation_authority() {
    let wallet = wallet();
    let change = Change::Revoke;

    assert_eq!(change.describe(), NoteVm::plain("change-revoke"));
    assert!(
        change.needs_typed_confirmation(),
        "a revocation would be sent without anybody typing the word",
    );

    let subject = wallet.address_of("ward@");
    let Prepared::Revoked(unsent) = prepared(&wallet, "ward@", &change) else {
        panic!("a revocation did not go through the revocation flow");
    };
    assert_eq!(
        unsent.outcome.authority, subject,
        "the revocation names an authority that is not the one the chain holds",
    );
}

/// Bringing a revoked identity back, and *only* that.
///
/// `replaces_primary_addresses` is the assertion that matters. A recovery may
/// legitimately hand the identity to new keys — that is usually the point of
/// recovering — and this wallet deliberately passes an empty change so it does
/// not. If that empty change is ever dropped, the identity is signed over to
/// whatever the SDK defaults to and the review screen says nothing about it.
#[test]
fn a_recovery_clears_the_revocation_and_hands_the_identity_to_nobody() {
    let wallet = wallet();
    let change = Change::Recover;

    assert_eq!(change.describe(), NoteVm::plain("change-recover"));
    assert!(!change.needs_typed_confirmation());

    let guardian = wallet.address_of("demo@");
    let Prepared::Recovered(unsent) = prepared(&wallet, "strayed@", &change) else {
        panic!("a recovery did not go through the recovery flow");
    };
    assert_eq!(
        unsent.outcome.authority, guardian,
        "the recovery was signed against an authority the chain does not name",
    );
    assert!(
        !unsent.outcome.replaces_primary_addresses,
        "recovering an identity replaced the keys that control it",
    );
}

/// The default a registration lands on, refused before a signature exists.
///
/// Every identity the demo script seeds is its own recovery authority, and
/// `maker@` still is. A revocation of one of those is unrecoverable by anybody,
/// so the builder refuses it — which is the rule `identity::detail` reports as
/// `cannot_be_revoked` on the detail screen, asserted here against the code
/// that enforces it rather than against a second copy of the rule.
#[test]
fn an_identity_that_is_its_own_recovery_authority_cannot_be_revoked() {
    let wallet = wallet();

    let refused = identity::prepare(
        &wallet.chain,
        &wallet.vault,
        LABEL,
        &wallet.address_of("maker@"),
        &Change::Revoke,
    )
    .err()
    .expect("a revocation into a dead end was signed");

    assert!(
        refused.to_lowercase().contains("strand"),
        "the refusal did not say what was wrong: {refused:?}",
    );
    assert_eq!(
        wallet.mock.broadcast_attempts(),
        0,
        "a refused revocation still reached the network",
    );
}

/// The last step, and the only one that changes anything.
///
/// The scripted chain refuses it, which is the point: what is asserted is that
/// the refusal is a **refusal**. `verus-flows` sorts a broadcast failure into
/// "the node said no" and "nobody knows", and everything it does not recognise
/// lands in the second — where the wallet writes a pending row and starts
/// polling for a transaction that was never accepted anywhere. For a revocation
/// that is the difference between a wallet that says no and one that offers to
/// revoke the identity again.
#[test]
fn confirming_a_change_reaches_the_network_exactly_once_and_the_refusal_is_a_refusal() {
    let wallet = wallet();
    let built = prepared(&wallet, "ward@", &Change::Revoke);

    let permit = wallet.nodes.spend_permit().expect("the guard is satisfied");
    let error = built
        .broadcast(&wallet.chain.broadcaster(&permit))
        .expect_err("the scripted chain broadcast a revocation");

    assert!(
        !matches!(error, verus_flows::FlowError::BroadcastUncertain { .. }),
        "a scripted revocation reported an unknown outcome, which sends the wallet \
         down the resolve-then-resend path for a revocation that never happened",
    );
    assert_eq!(
        wallet.mock.broadcast_attempts(),
        1,
        "a confirmed change was sent more or less than once",
    );
}
