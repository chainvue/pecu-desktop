//! Launching a currency from an identity this wallet already holds, end to
//! end, against a chain that cannot relay anything.
//!
//! # What this covers that `currency_build.rs` cannot
//!
//! That one builds definitions and hands them to the SDK's serialiser, which
//! proves the twelve fields this wallet fills in are a shape the chain accepts.
//! It never calls `currency::prepare`, so everything between the definition and
//! a signed transaction — opening the key by label, handing the flow the right
//! identity, reading the fee back off the outcome the review is drawn from —
//! was written and never run.
//!
//! The plan's gate for this stage is a real launch on VRSCTEST. That costs two
//! hundred coins and cannot be undone, so it is not something a test suite
//! does. This is the half that can be had for nothing: the same code path, the
//! same builder, the same signature, against a scripted chain — stopping one
//! step short of the network.
//!
//! `docs/LATER.md` §0b records that gate and what closing it has to show,
//! beside the identity gate it rides with: both are what keep the Currencies
//! and Profile screens out of the rail, and one criterion covers the pair.
//!
//! # Zero broadcasts, measured rather than argued
//!
//! `currency::prepare` is handed a `ChainReader` and no `Broadcaster`, so it is
//! incapable of sending. That is a property of the types and it is worth
//! asserting anyway, because the interesting failure is not "the flow sent
//! something" — it is a future refactor that gives the preparation step a
//! broadcaster for some unrelated convenience. `MockChain` counts attempts, and
//! the count is zero until this test asks for one on purpose.

#![cfg(feature = "mock")]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use pecu_core::currency;
use pecu_keystore::{NewKey, Vault};
use pecu_protocol::Secret;
use verus_sdk::money::Amount;
use verus_sdk::network::ChainReader;
use verus_sdk::verus_keys::{Address, PrivateKey};

/// From the SDK's own fixtures, so the address is a known quantity.
const WIF: &str = "UusoQWsobQKUkezgBJa22D9G4t9Avo6k8wD5UUxmmfAEoTN8bawc";

/// The one identity in the scripted chain a currency can be launched from.
const NAME: &str = "maker";

/// Coins, as the launch fee is written down.
fn coins(count: u64) -> Amount {
    Amount::from_sat(count * 100_000_000)
}

struct Wallet {
    vault: Vault,
    chain: pecu_chain::Chain,
    mock: pecu_mock::MockChain,
    nodes: pecu_chain::NodeManager,
    /// The temporary directory the vault lives in. Held so it outlives the
    /// vault rather than being dropped at the end of the setup function.
    _dir: tempfile::TempDir,
}

/// A wallet holding the key the scripted chain's identities are controlled by,
/// pointed at that chain.
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
        .add_key("funding", NewKey::FromWif { key })
        .expect("the key is added");

    let mock = pecu_mock::MockChain::demo(std::slice::from_ref(&address))
        .expect("the demo script builds");
    let chain = pecu_chain::Chain::Mock(mock.clone());

    // A permit needs a node that has answered, and the answer has to name the
    // chain the wallet is set to. Same shape `demo_chain.rs` uses.
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

/// The chain's own currency, which every definition here is parented to.
fn parent(chain: &pecu_chain::Chain) -> verus_sdk::currency::CurrencyId {
    let info = chain.chain_info().expect("the scripted chain answers");
    verus_sdk::currency::CurrencyId::from_bytes(
        info.chain_id
            .parse::<Address>()
            .expect("the chain's own id is an address")
            .hash(),
    )
}

fn draft(identity: &str) -> pecu_protocol::CurrencyDraft {
    pecu_protocol::CurrencyDraft {
        kind: "token".to_string(),
        identity: identity.to_string(),
        new_name: String::new(),
        // Mintable, so this is not the token that can never hold anything —
        // the shape `problems` refuses and the one it is easiest to launch by
        // accident.
        mintable: true,
        start_delay: "20".to_string(),
        reserves: Vec::new(),
        preallocations: Vec::new(),
    }
}

/// The whole of path B: an identity that exists, a draft, a signature, and a
/// review whose figures come off the transaction rather than off the form.
#[test]
fn a_launch_is_built_and_signed_from_an_identity_the_wallet_holds() {
    let wallet = wallet();

    let record = wallet
        .chain
        .identity(&format!("{NAME}@"))
        .expect("the scripted chain knows the identity it seeded");
    let tip = wallet.chain.block_count().expect("the chain has a tip");
    let start = u64::from(tip) + 20;

    let built = currency::definition(
        &draft(&record.identity_address),
        NAME,
        parent(&wallet.chain),
        start,
        &std::collections::BTreeMap::new(),
    )
    .expect("the draft builds a definition");

    let prepared = currency::prepare(
        &wallet.chain,
        &wallet.vault,
        "funding",
        &record.identity_address,
        &built,
    )
    .expect("the launch builds and signs against the scripted chain");

    assert_eq!(
        wallet.mock.broadcast_attempts(),
        0,
        "preparing a launch reached the network",
    );

    // The review is drawn from these three, and all three come off the signed
    // outcome. The fee is the chain's own policy figure — 200 on VRSCTEST —
    // read from the parent rather than assumed by the wallet.
    assert_eq!(prepared.launch_fee(), coins(200));
    assert_eq!(prepared.start_block(), start);
    assert_eq!(prepared.name, NAME);

    let split = currency::cost(prepared.launch_fee());
    assert_eq!(
        split.deposit.to_sat() + split.burned.to_sat(),
        split.launch_fee.to_sat(),
        "the two halves the review shows do not add back to the fee it shows",
    );
    assert_eq!(split.burned, coins(100));
}

/// The last step, and the only one that costs anything.
///
/// The scripted chain refuses it, which is the whole point: what is asserted is
/// that the refusal is a **refusal**. `verus-flows` sorts a broadcast failure
/// into "the node said no" and "nobody knows", and everything it does not
/// recognise lands in the second — where the wallet writes a pending row and
/// starts polling for a transaction that was never accepted anywhere.
///
/// For a two-hundred-coin launch that is the difference between a wallet that
/// says "no" and one that offers to send it again.
#[test]
fn confirming_reaches_the_network_exactly_once_and_the_refusal_is_a_refusal() {
    let wallet = wallet();

    let record = wallet
        .chain
        .identity(&format!("{NAME}@"))
        .expect("the scripted chain knows the identity it seeded");
    let tip = wallet.chain.block_count().expect("the chain has a tip");

    let built = currency::definition(
        &draft(&record.identity_address),
        NAME,
        parent(&wallet.chain),
        u64::from(tip) + 20,
        &std::collections::BTreeMap::new(),
    )
    .expect("the draft builds a definition");

    let prepared = currency::prepare(
        &wallet.chain,
        &wallet.vault,
        "funding",
        &record.identity_address,
        &built,
    )
    .expect("the launch builds and signs");

    let permit = wallet.nodes.spend_permit().expect("the guard is satisfied");
    let error = prepared
        .broadcast(&wallet.chain.broadcaster(&permit))
        .err()
        .expect("the scripted chain broadcast a currency launch");

    assert!(
        !matches!(error, verus_flows::FlowError::BroadcastUncertain { .. }),
        "a scripted launch reported an unknown outcome, which sends the wallet \
         down the resolve-then-resend path for a launch that never happened",
    );
    assert_eq!(
        wallet.mock.broadcast_attempts(),
        1,
        "a confirmed launch was sent more or less than once",
    );
}

/// A start block that is not ahead of the tip is refused before it is signed.
///
/// The wallet checks this itself, in `currency::problems`, and that check runs
/// on every keystroke against a tip that was read some seconds ago. This is the
/// same rule at the other end — applied by the flow, against the tip as it is
/// at the moment of signing — and it is the one that matters, because the tip
/// moves while somebody fills in a form.
///
/// Refused rather than signed matters here for a specific reason: a launch
/// whose start block has passed is accepted by the builder and rejected at
/// consensus, which is the expensive order to find out in.
#[test]
fn a_launch_that_starts_in_the_past_is_refused_before_a_signature() {
    let wallet = wallet();

    let record = wallet
        .chain
        .identity(&format!("{NAME}@"))
        .expect("the scripted chain knows the identity it seeded");
    let tip = wallet.chain.block_count().expect("the chain has a tip");

    let built = currency::definition(
        &draft(&record.identity_address),
        NAME,
        parent(&wallet.chain),
        // Behind the tip. Nothing in the form can produce this, and the tip
        // moving while a review is open can.
        u64::from(tip) - 5,
        &std::collections::BTreeMap::new(),
    )
    .expect("the draft builds a definition");

    let refused = currency::prepare(
        &wallet.chain,
        &wallet.vault,
        "funding",
        &record.identity_address,
        &built,
    )
    .err()
    .expect("a launch starting in the past was signed");

    assert!(
        refused.contains("tip"),
        "the refusal did not say what was wrong: {refused:?}",
    );
    assert_eq!(
        wallet.mock.broadcast_attempts(),
        0,
        "a refused launch still reached the network",
    );
}

/// The scripted chain offers exactly one identity a currency can be defined
/// under, and says why about the rest.
///
/// # Why this is a test and not a screenshot
///
/// Because the picker is only useful if something in it can be chosen, and for
/// most of this project's life nothing in the demo build could: `demo@` defines
/// a currency, `vault@` is timelocked, `gone@` is revoked and `stranger@` is
/// somebody else's. Four identities, four refusals, and a form that could be
/// opened and never completed without a node.
///
/// This walks the same three inputs `Core::refresh_currencies` collects — the
/// name, whether this wallet signs for it, and its status — through the same
/// function, and asserts what the screen would show.
#[test]
fn the_scripted_chain_offers_one_identity_and_explains_the_others() {
    let wallet = wallet();

    // As the identities screen reports them. `stranger@` is deliberately not
    // here: this list is what `Core` walks, and it walks the identities this
    // wallet's keys control.
    let mine = [
        (format!("{NAME}@"), "Active"),
        ("demo@".to_string(), "Active"),
        ("vault@".to_string(), "Locked"),
        ("gone@".to_string(), "Revoked"),
    ];

    let mut offered = Vec::new();
    for (typed, status) in &mine {
        let record = wallet
            .chain
            .identity(typed)
            .expect("the scripted chain knows the identity it seeded");
        let lookup = currency::classify(wallet.chain.currency_definition(&record.identity_address));
        let refusal = currency::refusal(&lookup, true, status);
        if refusal.code.is_empty() {
            offered.push(typed.clone());
        }
    }

    assert_eq!(
        offered,
        vec![format!("{NAME}@")],
        "the scripted chain does not offer exactly the one identity that can define a currency",
    );
}
