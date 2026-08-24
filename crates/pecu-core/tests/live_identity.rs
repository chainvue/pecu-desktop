//! The five identity changes on a real chain, in the only order they can go in.
//!
//! # Why this exists when `identity_changes.rs` passes
//!
//! Because that one builds and signs against a scripted chain and asserts —
//! correctly — that **nothing was broadcast**. Every check it makes is about a
//! transaction that never left the process. `docs/LATER.md` §0 names the gap it
//! leaves as the thing that keeps Profile out of the rail: locking, unlocking,
//! revoking and recovering each change an identity in a way a test double
//! cannot vouch for, and no test in this workspace had ever sent one. §0b
//! carries the exit criterion this file is half of.
//!
//! **Nothing in this repository claims this has been run.** It is written,
//! `#[ignore]`d, and red until somebody with a funded testnet wallet runs it —
//! at which point the result belongs in §0b beside the payment §0 records:
//! five txids, the blocks they confirmed in, and the answer to the question in
//! step 4.
//!
//! Green is not that record. Every precondition this file cannot satisfy is a
//! *skip*, which returns early from a test that then reports as passing —
//! deliberately, so that running the whole suite with `--ignored` on an
//! unprovisioned machine does not fail. A green `live_identity` with nothing
//! set is evidence of nothing. The txids are the evidence.
//!
//! # Why one test and not five
//!
//! They chain, and the order is forced rather than chosen. A revoked identity
//! cannot be updated at all, so authorities, lock and unlock have to come
//! before the revocation; a recovery needs something revoked to act on, so it
//! has to come after. That leaves exactly one sequence:
//!
//! 1. **authorities** — written back to the values already on the chain. A real
//!    transaction with no net effect, which is what makes this whole file
//!    re-runnable.
//! 2. **lock** — a delay that has not started.
//! 3. **unlock** — starts the countdown. It does *not* unlock anything; see
//!    `identity::unlock_note`.
//! 4. **revoke** — and this is the step nothing in this tree can answer in
//!    advance. Whether consensus accepts a revocation of an identity that is
//!    still counting down is not written down anywhere here, so this asks the
//!    chain and reports the answer either way, waiting the countdown out if the
//!    first attempt is refused.
//! 5. **recover** — clears the revocation and nothing else, which is what this
//!    wallet deliberately offers. The identity ends where it started, still
//!    carrying whatever countdown step 3 published.
//!
//! # If a run dies between step 4 and step 5
//!
//! The identity is left revoked, and a revoked identity cannot be updated — so
//! starting again at step 1 would panic on step 1 and never reach the recovery.
//! This file is therefore its own way out: it reads the subject before it
//! builds anything, and a subject that is **already revoked** sends it straight
//! to step 5 and nowhere else. Run it once to recover, then again for the
//! sequence.
//!
//! # What the operator has to provision
//!
//! Two identities, not one, and this is the part that cannot be automated:
//!
//! * `PECU_LIVE_IDENTITY` — the subject. Its primary address must be the
//!   address of `PECU_LIVE_WIF`, so this wallet can sign for it.
//! * **Its recovery authority must be a second identity**, not itself, and one
//!   the same key also controls. `verus-tx-identity` refuses a revocation whose
//!   subject is its own recovery authority before a signature exists, and
//!   consensus refuses it too: there would be nobody left who could recover it.
//!   A freshly registered identity *is* its own recovery authority, so a name
//!   claimed for this run has to have step 1 pointed somewhere else by hand
//!   first — and the second identity has to list the same key among its primary
//!   addresses, or step 5 cannot be signed.
//!
//! Both of those are checked here **before the first transaction is built**,
//! because the one that is not checkable afterwards is the recovery: it is
//! discovered at build time in step 5, after step 4 has already revoked the
//! identity for real. Two cheap reads buy a skip instead of a wallet stuck in a
//! state this harness cannot leave.
//!
//! Give it a name whose loss would not matter. Step 4 revokes it for real.
//!
//! ```sh
//! export PECU_LIVE_SEND=1
//! export PECU_LIVE_WIF=<a funded VRSCTEST WIF>        # in your own shell
//! export PECU_LIVE_IDENTITY=<the subject's i-address, or name@>
//! cargo test -p pecu-core --test live_identity -- --ignored --nocapture
//! ```
//!
//! A run in which every step is mined promptly is a matter of minutes. The
//! declared ceilings are much larger, and the worst case is ninety-five: ten
//! minutes each for steps 1, 2, 3 and 5, and fifty-five for step 4 if the chain
//! refuses the first revocation and the countdown — whose floor is the
//! transaction expiry, twenty blocks — has to be waited out. Start it with that
//! in mind rather than with an hour in mind.
//!
//! Nothing here prints the key, and nothing writes it anywhere: the vault it
//! builds lives in a temporary directory that is removed when the test ends.

// `panic!` is right in the polling helper: giving up on a confirmation is a
// failed test, and returning a flag would let the steps after it run against an
// identity whose earlier change never landed.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::time::{Duration, Instant};

use pecu_chain::{Chain, Network, Node, NodeManager};
use pecu_core::identity::{self, Change};
use pecu_keystore::{NewKey, Vault};
use pecu_protocol::Secret;
use verus_sdk::money::{Amount, Txid};
use verus_sdk::network::{ChainReader, FlowError};
use verus_sdk::verus_keys::PrivateKey;

const TESTNET: &str = "https://api.verustest.net";
const LABEL: &str = "live";

/// Five transactions, each paying a miner fee, plus room to be wrong about the
/// fee. Small on a chain whose coins are free, and the point of checking is to
/// fail with "fund it" rather than three steps later with "insufficient funds".
const NEEDED_SATS: u64 = 1_000_000;

/// The delay the lock publishes. Any figure does — the wait that follows is set
/// by the transaction's expiry, not by this.
const LOCK_DELAY: u32 = 10;

/// How long to wait for a change to be mined before giving up.
const CONFIRM_TIMEOUT: Duration = Duration::from_mins(10);

/// How long to wait for a countdown to elapse, if step 4 has to.
///
/// Longer than the rest by a lot, and not arbitrarily: an unlock publishes a
/// height of at least `delay + expiry`, and the expiry floor is twenty blocks.
/// A ceiling under that would be a timeout that can only ever fire.
const UNLOCK_TIMEOUT: Duration = Duration::from_mins(45);

/// The output currently holding an identity. It moves with every change, which
/// is the only thing that distinguishes "mined" from "not sent yet".
type Held = (Txid, u32);

/// Why a step produced no confirmed transaction.
///
/// # Why the two are not one string
///
/// Because step 4 reports one of them as the chain's verdict on a question
/// nobody here can answer, and reporting the other that way would be a lie
/// with an hour of waiting attached. A wallet-side refusal —
/// `RevocationWouldStrand`, an authority the keys do not satisfy, a vault that
/// will not open — happened before anything was sent. Consensus was never
/// asked, so it said nothing, and retrying after a countdown cannot help.
enum Refused {
    /// The wallet would not build it. Nothing reached the network.
    Locally(String),
    /// The node was asked and answered.
    ByTheChain(FlowError),
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Locally(reason) => write!(f, "the wallet would not build it: {reason}"),
            Self::ByTheChain(error) => write!(f, "the chain refused it: {error}"),
        }
    }
}

/// The key, or `None` with a reason printed.
///
/// Returned rather than `expect`ed so the ordinary case — somebody running the
/// whole suite — is a skip with an explanation, not a failure.
fn funded_key() -> Option<PrivateKey> {
    if std::env::var("PECU_LIVE_SEND").as_deref() != Ok("1") {
        eprintln!(
            "skipping: this test spends and revokes. Set PECU_LIVE_SEND=1, \
             PECU_LIVE_WIF=<funded WIF> and PECU_LIVE_IDENTITY=<the subject>."
        );
        return None;
    }
    let wif =
        std::env::var("PECU_LIVE_WIF").expect("PECU_LIVE_SEND is set but PECU_LIVE_WIF is not");
    Some(PrivateKey::from_wif(&wif).expect("PECU_LIVE_WIF is not a WIF this chain can read"))
}

/// The identity to work on, named the strong way, with its authorities.
struct Subject {
    /// The i-address, whatever the operator typed. `current_identity` can check
    /// an i-address against the object it decodes with no node involved; a
    /// `name@` lookup can only be checked against what the node itself said.
    address: String,
    revocation: String,
    recovery: String,
    /// What the wallet's own list called it before anything was built. Step 1
    /// waits for this rather than for "Active": writing the authorities back
    /// changes no state, so the state it should end in is the one it was in.
    status: String,
}

/// Can this one key satisfy the authority the identity at `address` names?
///
/// The same question `check_authority` asks at build time — is the key among
/// that identity's primary addresses, and is one signature enough — asked
/// before anything is broadcast rather than after. For the recovery authority
/// that difference is the whole point: build time for a recovery is step 5,
/// which runs after step 4 has revoked the identity for real, and a wallet that
/// cannot sign the recovery has no way to put it back.
///
/// A read failure is treated as "cannot", not as a test failure: a recovery
/// authority the chain does not know is a provisioning mistake, which is
/// somebody's wallet to fix and not this code's to be right about.
fn one_key_satisfies(chain: &Chain, address: &str, signer: &str, what: &str, step: u8) -> bool {
    let record = match chain.identity(address) {
        Ok(record) => record,
        Err(error) => {
            eprintln!("skipping: the chain does not know the {what} authority {address}: {error}");
            return false;
        }
    };

    let min_sigs = record.identity["minimumsignatures"].as_u64().unwrap_or(1);
    if min_sigs > 1 {
        eprintln!(
            "skipping: the {what} authority {address} needs {min_sigs} signatures and this \
             wallet signs with one key.",
        );
        return false;
    }

    let holds = record.identity["primaryaddresses"].as_array().is_some_and(|all| {
        all.iter()
            .filter_map(|one| one.as_str())
            .any(|one| one == signer)
    });
    if !holds {
        eprintln!(
            "skipping: PECU_LIVE_WIF is not among the primary addresses of the {what} \
             authority {address}, so step {step} could not be signed. Point it at an \
             identity this key controls and run again.",
        );
    }
    holds
}

/// The subject, or `None` with a reason printed — including the provisioning
/// mistakes, which are skips rather than failures because they are somebody's
/// wallet to fix and not this code's to be right about.
fn subject(chain: &Chain, signer: &str, tip: u32) -> Option<Subject> {
    let typed = match std::env::var("PECU_LIVE_IDENTITY") {
        Ok(typed) if !typed.trim().is_empty() => typed,
        _ => {
            eprintln!("skipping: PECU_LIVE_IDENTITY is not set. See this file's header.");
            return None;
        }
    };

    let record = chain
        .identity(&typed)
        .unwrap_or_else(|error| panic!("the chain does not know {typed}: {error}"));
    let row = identity::row_of(&record, tip, std::slice::from_ref(&signer.to_string()));
    println!("subject   {} ({})", row.name, record.identity_address);
    println!("status    {}", row.status);

    if !row.mine {
        eprintln!(
            "skipping: PECU_LIVE_WIF is not among {}'s primary addresses, so this wallet \
             cannot sign for it.",
            row.name,
        );
        return None;
    }

    let field = |key: &str| {
        record.identity[key]
            .as_str()
            .unwrap_or_default()
            .to_string()
    };
    let (revocation, recovery) = (field("revocationauthority"), field("recoveryauthority"));
    println!("revocation {revocation}");
    println!("recovery   {recovery}");

    if recovery == record.identity_address {
        eprintln!(
            "skipping: {} is its own recovery authority, so a revocation of it is refused \
             before a signature exists — by this wallet's SDK and by consensus. Point its \
             recovery authority at a second identity the same key controls and run again.",
            row.name,
        );
        return None;
    }

    if !one_key_satisfies(chain, &revocation, signer, "revocation", 4)
        || !one_key_satisfies(chain, &recovery, signer, "recovery", 5)
    {
        return None;
    }

    Some(Subject {
        address: record.identity_address,
        revocation,
        recovery,
        status: row.status,
    })
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

/// What the wallet's own list would call this identity right now, and which
/// output is holding it — or `None`, with the reason printed.
///
/// Read through `identity::row_of` rather than off the JSON, because the status
/// is a derivation — a locked identity and one whose countdown has elapsed
/// carry the same `unlock_after` and are not the same state — and the thing
/// worth confirming against a real chain is the wallet's reading of it, not the
/// daemon's fields.
///
/// `None` rather than a panic because this runs on a loop against a public
/// endpoint for as long as an hour and a half. One timed-out request is not an
/// answer about the identity, and treating it as one would abandon the run at
/// whatever step it happened to be on — including between the revocation and
/// the recovery, which is the window this file has no other way out of.
fn state_of(chain: &Chain, address: &str) -> Option<(String, Held)> {
    let record = match chain.identity(address) {
        Ok(record) => record,
        Err(error) => {
            println!("  (could not read {address}: {error})");
            return None;
        }
    };
    let tip = match chain.block_count() {
        Ok(tip) => tip,
        Err(error) => {
            println!("  (could not read the tip: {error})");
            return None;
        }
    };
    Some((identity::row_of(&record, tip, &[]).status, record.outpoint))
}

/// Poll the subject until `settled` is happy with it, or give up at `ceiling`.
fn poll(
    chain: &Chain,
    address: &str,
    ceiling: Duration,
    settled: impl Fn(&str, Held) -> bool,
) -> bool {
    let started = Instant::now();
    let mut last = String::new();
    while started.elapsed() < ceiling {
        if let Some((now, held)) = state_of(chain, address) {
            if now != last {
                println!("  now {now} ({:?} elapsed)", started.elapsed());
                last.clone_from(&now);
            }
            if settled(&now, held) {
                return true;
            }
        }
        std::thread::sleep(Duration::from_secs(15));
    }
    false
}

/// Poll until the change that spent `from` has been mined and left the identity
/// reading as one of `want`.
///
/// Both halves are load-bearing, and the outpoint is the half that is easy to
/// leave out. Three of the five changes here end in a state the identity was
/// **already in** — writing the authorities back changes nothing, and a
/// recovery leaves whatever countdown was running — so a wait on the state
/// alone returns immediately, before the transaction is mined, and the next
/// step is then built against the outpoint this one is spending. That is the
/// failure `live_shielded_spend.rs:100` records at length, arrived at from the
/// other direction. The output holding an identity moves with every change, so
/// "not the one we started from" is the one predicate that is true for all five.
fn wait_for_change(
    chain: &Chain,
    address: &str,
    from: Held,
    want: &[&str],
    ceiling: Duration,
) -> bool {
    poll(chain, address, ceiling, |state, held| {
        held != from && want.contains(&state)
    })
}

/// Poll until the identity reads as one of `want`, however it got there.
///
/// For the one wait that is not about a transaction: a countdown elapsing moves
/// no output and spends nothing. Only the tip has to change.
fn wait_for_state(chain: &Chain, address: &str, want: &[&str], ceiling: Duration) -> bool {
    poll(chain, address, ceiling, |state, _| want.contains(&state))
}

/// Where the chain is holding the identity, insisted on rather than polled.
///
/// Read before every send, because it is what the wait afterwards compares
/// against. A failure here is a failure to start, not a failure to confirm.
fn held_at(chain: &Chain, address: &str) -> Held {
    chain
        .identity(address)
        .unwrap_or_else(|error| panic!("reading {address} before sending: {error}"))
        .outpoint
}

/// Prepare, broadcast and confirm one change — the whole of what the wallet
/// does when somebody presses the button.
///
/// Returns the refusal rather than asserting on it, because step 4 has a
/// legitimate reason to be refused and wants to say which kind it was.
fn send(
    chain: &Chain,
    nodes: &NodeManager,
    vault: &Vault,
    address: &str,
    change: &Change,
) -> Result<String, Refused> {
    println!("\n── {change:?}");
    let prepared =
        identity::prepare(chain, vault, LABEL, address, change).map_err(Refused::Locally)?;
    println!("  fee {}", prepared.fee().to_coins_string());
    println!("  the review would say {:?}", change.describe());
    if change.needs_typed_confirmation() {
        println!(
            "  and would hold it behind typing {:?}",
            identity::REVOKE_CONFIRMATION,
        );
    }

    let permit = nodes
        .spend_permit()
        .expect("a healthy testnet node must yield a permit");
    let txid = prepared
        .broadcast(&chain.broadcaster(&permit))
        .map_err(Refused::ByTheChain)?;
    println!("  txid {txid}");

    // The chain's own word for it, which is the only evidence that counts.
    let seen = chain
        .confirmations(&txid)
        .expect("asking the node about a txid it just took");
    assert!(
        seen.is_some(),
        "the node accepted {txid} and then did not know it",
    );
    Ok(txid)
}

/// The same, for the four steps that have no business being refused.
fn must_send(
    chain: &Chain,
    nodes: &NodeManager,
    vault: &Vault,
    address: &str,
    change: &Change,
    want: &[&str],
) {
    let from = held_at(chain, address);
    if let Err(refused) = send(chain, nodes, vault, address, change) {
        panic!("{change:?} was refused: {refused}");
    }
    assert!(
        wait_for_change(chain, address, from, want, CONFIRM_TIMEOUT),
        "{change:?} was accepted and the identity never read as one of {want:?} at a new output",
    );
}

/// Revoke it, and answer the question this tree does not answer.
///
/// The identity is counting down when this runs, and whether consensus permits
/// a revocation in that state is written down nowhere in this repository. So it
/// is asked rather than assumed: if the *chain* refuses it, the refusal is
/// printed, the countdown is waited out, and it is asked again. Either way the
/// run learns something worth writing into `docs/LATER.md` §0b — and either way
/// step 5 still gets a revoked identity to recover.
///
/// A refusal the wallet produced before sending is not an answer and is not
/// reported as one. Nothing was broadcast, consensus was never asked, and
/// waiting three quarters of an hour to ask a second time would not change a
/// decision made in this process.
fn revoke(chain: &Chain, nodes: &NodeManager, vault: &Vault, address: &str) {
    let from = held_at(chain, address);
    let refused = match send(chain, nodes, vault, address, &Change::Revoke) {
        Ok(_) => {
            assert!(
                wait_for_change(chain, address, from, &["Revoked"], CONFIRM_TIMEOUT),
                "the revocation was accepted and the identity never read as Revoked",
            );
            println!(
                "\nANSWER: a revocation of an identity that is still counting down IS accepted."
            );
            return;
        }
        Err(Refused::Locally(reason)) => panic!(
            "the wallet refused to build the revocation, so the chain was never asked and \
             this run answers nothing: {reason}",
        ),
        Err(Refused::ByTheChain(error)) => error,
    };

    println!("\nANSWER: a revocation of an identity that is still counting down is refused.");
    println!("  the chain said: {refused}");
    assert!(
        !matches!(refused, FlowError::BroadcastUncertain { .. }),
        "the revocation reported an unknown outcome, which sends the wallet down the \
         resolve-then-resend path for a revocation that may already be propagating: {refused}",
    );

    println!("\nwaiting for the countdown to elapse before trying again");
    assert!(
        wait_for_state(chain, address, &["Active"], UNLOCK_TIMEOUT),
        "the countdown never elapsed, so the revocation could not be retried",
    );
    must_send(chain, nodes, vault, address, &Change::Revoke, &["Revoked"]);
}

/// Put it back, and accept any state it can legally come back in.
///
/// The assertion that matters is that it is no longer `Revoked`, and the three
/// states listed are `identity::Status` minus that one — written out rather
/// than negated because `wait_for_change` takes the wallet's own words for
/// them.
///
/// Naming `Active` alone would be wrong, and quietly. A recovery clears
/// `FLAG_REVOKED` and nothing else — deliberately, because the alternative is
/// signing the identity over to whatever the SDK defaults to — so the countdown
/// step 3 published survives it. If step 4's revocation was accepted while the
/// identity was still counting down, twenty-odd blocks of that countdown remain
/// when step 5 lands and the wallet reads the recovered identity as
/// `Unlocking`; only if the countdown had to be waited out first does it read
/// as `Active`. Insisting on `Active` would sit out the ten-minute ceiling in
/// the branch this file exists to explore, and then report a recovery that
/// worked as one that never landed.
fn recover(chain: &Chain, nodes: &NodeManager, vault: &Vault, address: &str) {
    must_send(
        chain,
        nodes,
        vault,
        address,
        &Change::Recover,
        &["Active", "Unlocking", "Locked"],
    );
}

/// Every change this wallet offers, against the chain, in the order they can go.
#[ignore = "spends real coins on VRSCTEST and revokes a real identity"]
#[test]
fn every_identity_change_this_wallet_offers_is_accepted_by_the_network() {
    let Some(key) = funded_key() else { return };

    let signer = key.address().to_string();
    let chain = Chain::live(TESTNET).expect("a client for the testnet endpoint");
    let (nodes, tip) = permitted(&chain);
    println!("signer    {signer}");
    println!("tip       {tip}");

    let Some(subject) = subject(&chain, &signer, tip) else {
        return;
    };

    // What can actually be spent, before anything is built. A test that fails
    // with "insufficient funds" four steps in has wasted an hour and looks like
    // a bug in the builder.
    let funding = verus_sdk::network::spendable(&chain, &signer).expect("the funding read");
    println!("spendable {}", funding.total.to_coins_string());
    if funding.total.to_sat() < NEEDED_SATS {
        eprintln!(
            "skipping: {signer} has {} and five changes need a little over {}. Fund it \
             and run again.",
            funding.total.to_coins_string(),
            Amount::from_sat(NEEDED_SATS).to_coins_string(),
        );
        return;
    }

    let (_dir, vault) = vault_holding(key);
    let at = subject.address.as_str();

    // An earlier run that died between step 4 and step 5. Nothing else can be
    // done to a revoked identity, so this is the only useful thing to do with
    // it — and doing it is how a stuck wallet gets unstuck. See the header.
    if subject.status == "Revoked" {
        println!("\nalready revoked — recovering it and stopping there");
        recover(&chain, &nodes, &vault, at);
        println!("\nrecovered. Run this again for the whole sequence.");
        return;
    }

    // 1. The authorities it already has, written back. A real transaction with
    //    no net effect, which is what lets this file be run twice — and why the
    //    state it should end in is the state it started in.
    must_send(
        &chain,
        &nodes,
        &vault,
        at,
        &Change::Authorities {
            revocation: Some(subject.revocation.clone()),
            recovery: Some(subject.recovery.clone()),
        },
        &[subject.status.as_str()],
    );

    // 2 and 3. Hold it, then start the clock. "Unlocking" rather than "Active"
    //    after step 3 is the whole point of `identity::unlock_note`: the
    //    transaction starts the countdown, it does not end the lock.
    must_send(
        &chain,
        &nodes,
        &vault,
        at,
        &Change::Lock { delay: LOCK_DELAY },
        &["Locked"],
    );
    must_send(
        &chain,
        &nodes,
        &vault,
        at,
        &Change::Unlock { extra_blocks: 0 },
        &["Unlocking"],
    );

    // 4 and 5. The two that cannot be tested any other way.
    revoke(&chain, &nodes, &vault, at);
    recover(&chain, &nodes, &vault, at);

    println!("\nthe revocation is cleared; this file can be run again");
}
