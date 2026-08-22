//! `t→z`, then `z→z`, then `z→t` — all three on the real chain, in one run.
//!
//! # Why one test and not three
//!
//! They chain. A `z→z` needs a note, and the only way this wallet gets one is by
//! shielding first; a `z→t` then spends what the `z→z` produced. Three separate
//! tests would each have to find the previous one's output, which means either
//! sharing state between tests or re-doing the earlier steps anyway.
//!
//! # What it needs
//!
//! It spends, so it is gated the same way `live_send.rs` is — twice, because a
//! WIF sitting in an environment is not by itself consent to spend from it. The
//! light server defaults to the one the chain ships; `PECU_LIGHT_URL` overrides
//! it.
//!
//! ```sh
//! export PECU_LIVE_SEND=1
//! export PECU_LIVE_WIF=<a funded VRSCTEST WIF>       # in your own shell
//! cargo test -p pecu-core --test live_shielded_spend -- --ignored --nocapture
//! ```
//!
//! Allow several minutes: each step waits for a block, and each shielded step
//! spends tens of seconds proving.
//!
//! # The shielded account is generated here, not yours
//!
//! `PECU_LIVE_WIF` is a WIF, and a WIF has no recovery phrase — so it can have
//! no shielded account, ever. This test generates its own phrase, shields *your
//! transparent coin into that account*, and spends from it. The phrase exists
//! for the length of the run and is never written down, which is fine: the
//! `z→t` at the end sends the remainder back to your transparent address, and
//! anything left behind is a testnet fraction of a coin.

// `panic!` is right in the polling helper: giving up on a confirmation is a
// failed test, and returning a flag would let the steps after it run against a
// balance that never arrived.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::time::{Duration, Instant};

use pecu_chain::{Chain, LightServer, Network, Node, NodeManager};
use pecu_core::{params, shield, shielded};
use pecu_keystore::ShieldedView;
use verus_sdk::money::Amount;
use verus_sdk::verus_keys::{bip39, PrivateKey};

const TESTNET: &str = "https://api.verustest.net";

/// What gets shielded. Enough to survive two more fees and still be above dust.
const SHIELD_SATS: u64 = 2_000_000;
/// What the `z→z` moves, and then what the `z→t` sends home.
const PRIVATE_SATS: u64 = 1_000_000;
const UNSHIELD_SATS: u64 = 400_000;

/// How long to wait for a transaction to be mined before giving up.
const CONFIRM_TIMEOUT: Duration = Duration::from_mins(10);

fn light() -> LightServer {
    let named = std::env::var("PECU_LIGHT_URL")
        .ok()
        .filter(|url| !url.is_empty());
    match named {
        Some(url) => LightServer::connect(&url, &Network::Testnet),
        None => LightServer::shipped(&Network::Testnet),
    }
    .expect("connect to the light server")
}

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

fn permitted(chain: &Chain) -> (NodeManager, u32) {
    let (info, latency) = chain.probe();
    let info = info.expect("the testnet endpoint answered getinfo");
    assert_eq!(info.name, "VRSCTEST", "this test only runs against testnet");

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

/// Scan forward until `ready` is satisfied, or give up.
///
/// # Why a predicate and not a balance
///
/// The first version waited for the balance to reach a figure, and that was
/// wrong in a way only a live run could show. After a `z→z` the balance is
/// **almost unchanged** — value moved inside the pool, so what comes back is
/// the payment plus the change, less the fee. "Balance is at least X" was
/// therefore already true of the state from *before* the spend, so the wait
/// returned at once, the next step planned against a stale scan, and the daemon
/// refused it with `bad-txns-sapling-nullifier-exists` after the prover had
/// been paid for.
///
/// What distinguishes before from after is the *shape*: one note becomes two.
/// So the caller says what it is waiting for.
///
/// Polls rather than sleeping a fixed time: testnet blocks are irregular, and a
/// fixed wait is either too short to be reliable or too long to sit through.
fn wait_until(
    watching: &mut shielded::Shielded,
    server: &LightServer,
    from: u64,
    what: &str,
    ready: impl Fn(&shielded::Shielded) -> bool,
) {
    let started = Instant::now();
    let mut last = (0, 0);
    while started.elapsed() < CONFIRM_TIMEOUT {
        let tip = server.synced_height().expect("tip");
        // A fresh scan each round rather than a continuation: the earlier rounds
        // saw a shorter chain, and starting over from `from` is cheap over the
        // handful of blocks this covers.
        let mut fresh = shielded::Shielded::watching(&view_of(watching)).expect("viewing key");
        if fresh.sync(server.client(), from, tip).is_ok() {
            let now = (fresh.balance(), fresh.note_count());
            if now != last {
                println!(
                    "  {what}: {} sat in {} note(s) at block {tip}",
                    now.0, now.1
                );
                last = now;
            }
            if ready(&fresh) {
                *watching = fresh;
                return;
            }
        }
        std::thread::sleep(Duration::from_secs(15));
    }
    panic!("{what}: gave up after {CONFIRM_TIMEOUT:?} — last seen {last:?}");
}

/// The viewing material of an account being watched, so a fresh scan can be
/// started from the same key.
fn view_of(watching: &shielded::Shielded) -> ShieldedView {
    watching.view().clone()
}

#[ignore = "spends real coins on VRSCTEST and takes minutes"]
#[test]
fn all_three_shielded_directions_are_accepted_by_the_network() {
    let Some(key) = funded_key() else { return };

    let chain = Chain::live(TESTNET).expect("a client for the testnet endpoint");
    let (nodes, tip) = permitted(&chain);
    let server = light();
    let from_address = key.address().to_string();

    let funding = verus_sdk::network::spendable(&chain, &from_address).expect("funding");
    println!("transparent {from_address}");
    println!("spendable   {}", funding.total.to_coins_string());
    println!("tip         {tip}");

    if funding.total.to_sat() < SHIELD_SATS * 3 {
        eprintln!(
            "skipping: {from_address} holds {} and this needs a little over {}.",
            funding.total.to_coins_string(),
            Amount::from_sat(SHIELD_SATS * 3).to_coins_string(),
        );
        return;
    }

    let located = params::find(
        &std::path::PathBuf::from(std::env::var("HOME").expect("HOME")).join(".pecu-not-here"),
    )
    .expect("the Sapling parameters must be on this machine");
    let sapling = params::load(&located).expect("the parameters load");

    // The account this test owns. Generated here — see the module docs on why a
    // WIF cannot have one.
    let entropy = pecu_keystore::entropy().expect("entropy");
    let phrase = bip39::mnemonic_from_entropy(&entropy);
    let view = pecu_keystore::with_spending_key(&phrase, |extsk| *extsk).expect("derive");
    let account = {
        let seed = bip39::mnemonic_to_seed(&phrase, "").expect("seed");
        verus_sdk::light::derive_account(seed.as_ref(), verus_sdk::light::COIN_TYPE_MAINNET, 0)
            .expect("account")
    };
    let zaddr = verus_sdk::light::zaddr::encode(&account.address).expect("encode");
    println!("shielded    {zaddr}");

    let start = u64::from(tip);

    // ── 1. t→z ──────────────────────────────────────────────────────────────
    println!("\n[1/3] t→z  shielding {SHIELD_SATS} sat");
    let planned = shield::plan(
        &chain,
        &from_address,
        &zaddr,
        &Amount::from_sat(SHIELD_SATS).to_coins_string(),
    )
    .expect("plan the shield");
    let proving = Instant::now();
    let prepared = shield::prepare_with_key(&key, &sapling, &planned).expect("prove and sign");
    println!("  proved in {:.1?}", proving.elapsed());
    let permit = nodes.spend_permit().expect("a permit for testnet");
    let txid = shield::broadcast(&chain, &permit, &prepared).expect("the network accepted it");
    println!("  txid {txid}");

    let mut watching = shielded::Shielded::watching(&ShieldedView {
        dfvk: account.dfvk,
        address: zaddr.clone(),
        diversifier_index: account.diversifier_index,
    })
    .expect("viewing key");
    wait_until(&mut watching, &server, start, "shielded balance", |s| {
        s.balance() >= SHIELD_SATS
    });
    println!("  confirmed: {} sat in the pool", watching.balance());

    // ── 2. z→z ──────────────────────────────────────────────────────────────
    println!("\n[2/3] z→z  paying {PRIVATE_SATS} sat to the same account");
    let plan = watching
        .plan_spend(&zaddr, &Amount::from_sat(PRIVATE_SATS).to_coins_string())
        .expect("plan the private spend");
    println!("  spending {} note(s)", plan.note_count());
    let proving = Instant::now();
    let unsent = shielded::prove_spend(server.client(), &chain, &sapling, &view, &plan)
        .expect("prove the shielded spend");
    println!("  proved in {:.1?}", proving.elapsed());
    let permit = nodes.spend_permit().expect("a permit");
    let spent = shielded::broadcast_spend(&chain, &permit, unsent).expect("accepted");
    println!("  txid {}", spent.txid);

    // The note is gone the moment the network takes it, and the chain will not
    // say so for another block. This is what stops the next step choosing it
    // again — the same call the actor makes when a broadcast is accepted.
    watching.note_spent(&plan);

    // One note became two: the payment to itself, and the change. That is the
    // shape that tells after from before — the balance barely moves, because
    // the value never left the pool.
    wait_until(&mut watching, &server, start, "after z→z", |s| {
        s.note_count() >= 2
    });

    // ── 3. z→t ──────────────────────────────────────────────────────────────
    println!("\n[3/3] z→t  sending {UNSHIELD_SATS} sat back to {from_address}");
    let plan = watching
        .plan_spend(
            &from_address,
            &Amount::from_sat(UNSHIELD_SATS).to_coins_string(),
        )
        .expect("plan the unshield");
    let proving = Instant::now();
    let unsent = shielded::prove_spend(server.client(), &chain, &sapling, &view, &plan)
        .expect("prove the unshield");
    println!("  proved in {:.1?}", proving.elapsed());
    let permit = nodes.spend_permit().expect("a permit");
    let spent = shielded::broadcast_spend(&chain, &permit, unsent).expect("accepted");
    println!("  txid {}", spent.txid);

    println!("\nall three accepted by VRSCTEST");
}
