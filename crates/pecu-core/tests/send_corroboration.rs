//! Issue #29's own scenario, end to end, against two scripted nodes.
//!
//! # What this is about
//!
//! `#28` made a node prove its name against its chain id. Both halves arrive in
//! the same `getinfo` reply, so a node willing to rewrite one is willing to
//! rewrite both — and the interesting version of that node does not rewrite
//! anything: it forwards `getinfo` to a real VRSCTEST daemon and answers
//! `getaddressutxos` from mainnet. Every identity check in the wallet passes,
//! and the transaction it then signs is consensus-valid on the chain the coins
//! actually came from, because Verus shares address version bytes across the two
//! and has no branch-id separation between them.
//!
//! A source cannot corroborate itself. The only thing that reaches this is
//! asking a second node about the same address, which is what
//! `pecu_chain::corroborate` does and what this file drives through
//! `send::prepare` — the function the Send screen actually calls.
//!
//! # Why two `MockChain`s and not two `ScriptedReader`s
//!
//! Because the *identity* half of the scenario has to be scriptable for the
//! scenario to be the one in the issue: the hostile node has to call itself
//! VRSCTEST, with the matching chain id, while serving another chain's coins.
//! `ScriptedReader::chain_info` is hard-coded and has no builder;
//! `MockState` has `chain_name`, `chain_id` and `utxos` as public fields, so
//! the whole shape is expressible. `MockChain::send_raw_transaction` also fails
//! unconditionally and counts attempts, so a test that gets this wrong cannot
//! broadcast and can prove it did not try.
//!
//! # Why the whole file is behind `--features mock`
//!
//! `send::prepare` takes a `pecu_chain::Chain`, and the only variant that
//! answers without a socket is `Chain::Mock`, which the feature gates. Run it
//! with `cargo test -p pecu-core --features mock`, which is what
//! `scripts/check.sh mock` does.

#![cfg(feature = "mock")]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use pecu_chain::{Chain, SpendRefused};
use pecu_core::send::{self, Corroborator};
use pecu_keystore::{NewKey, Vault};
use pecu_protocol::Secret;
use verus_sdk::money::{Amount, Txid, Utxo};
use verus_sdk::network::AddressUtxo;
use verus_sdk::verus_keys::PrivateKey;

/// From the SDK's own fixtures, so the address is a known quantity.
const WIF: &str = "UusoQWsobQKUkezgBJa22D9G4t9Avo6k8wD5UUxmmfAEoTN8bawc";
const LABEL: &str = "funding";
const COIN: u64 = 100_000_000;

/// Where the hostile node says it is, and the id that goes with that name.
///
/// Both correct, and that is the point: this node passes every check `#28`
/// added. What it lies about is the coins.
const TESTNET_ID: &str = "iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq";

/// The URL the shipped endpoint is named by on the review and in a refusal.
const SHIPPED_URL: &str = "https://api.verustest.net";

/// A distinct, well-formed transaction id per coin.
///
/// `chain` separates the two nodes' worlds: coins from different chains are
/// different outpoints, which is the fact the whole guard rests on.
fn txid(chain: u8, index: u8) -> Txid {
    let mut bytes = [0u8; 32];
    bytes[0] = chain;
    bytes[1] = index;
    bytes[31] = 0x5a;
    Txid::from_internal(bytes)
}

fn recipient() -> String {
    PrivateKey::from_bytes(&[2u8; 32], true)
        .expect("a fixed scalar is a valid key")
        .address()
        .to_string()
}

struct Wallet {
    vault: Vault,
    address: String,
    script: Vec<u8>,
    _dir: tempfile::TempDir,
}

fn wallet() -> Wallet {
    let dir = tempfile::tempdir().expect("tempdir");
    let passphrase = Secret::new("correct horse battery staple".to_string());
    let vault = Vault::create(&dir.path().join("vault.json"), "test", &passphrase)
        .expect("a fresh vault is created");
    vault
        .unlock(&passphrase)
        .expect("the vault just made opens");

    let key = PrivateKey::from_wif(WIF).expect("the fixture WIF is valid");
    let address = key.address();
    let script = address
        .p2pkh_script_pubkey()
        .expect("a pay-to-public-key-hash address has a script");
    vault
        .add_key(LABEL, NewKey::FromWif { key })
        .expect("the key is added");

    Wallet {
        vault,
        address: address.to_string(),
        script,
        _dir: dir,
    }
}

/// What the two chains in issue #29 had actually reached when this was written.
///
/// `api.verus.services` answered 4,207,412 blocks and `api.verustest.net`
/// 1,203,115. The figures are not decoration: the whole attack is a node
/// serving the first chain's outputs while calling itself the second, so its
/// coins sit three million blocks *above* anything the honest node has indexed.
/// A suite that gave both nodes the same tip — which is what this file did
/// first, by taking `MockState::default().tip` for every node — certified a
/// verdict the field never produces, and the field's verdict was the wrong one.
const MAINNET_TIP: u32 = 4_207_412;
const TESTNET_TIP: u32 = 1_203_115;

/// A node calling itself VRSCTEST, holding `coins` at the wallet's address, and
/// answering from `tip`.
///
/// The identity is the same on both nodes in every test here. That is
/// deliberate: if the two disagreed about the chain, `#28`'s check would catch
/// it and this file would be testing something already covered. What differs is
/// the height each one answers from, because that is the only thing left that
/// tells a hostile endpoint from an honest one.
fn node_at(wallet: &Wallet, chain: u8, coins: &[u64], tip: u32) -> pecu_mock::MockChain {
    let mut state = pecu_mock::MockState {
        chain_name: "VRSCTEST".to_string(),
        chain_id: TESTNET_ID.to_string(),
        tip,
        ..pecu_mock::MockState::default()
    };
    let held: Vec<AddressUtxo> = coins
        .iter()
        .enumerate()
        .map(|(index, satoshis)| AddressUtxo {
            utxo: Utxo {
                txid: txid(chain, u8::try_from(index).expect("a handful of coins fit a u8")),
                vout: 0,
                satoshis: Amount::from_sat(*satoshis),
                script_pubkey: wallet.script.clone(),
            },
            address: wallet.address.clone(),
            // Well past coinbase maturity, so nothing here is about the
            // maturity rule.
            height: tip - 500,
            is_spendable: true,
        })
        .collect();
    state.utxos.insert(wallet.address.clone(), held);
    pecu_mock::MockChain::new(state)
}

/// The same, at whatever tip the mock ships with.
///
/// For the tests where the two nodes' heights are not the subject — the default
/// install, the node that cannot answer — and where giving them different ones
/// would only make the fixture harder to read.
fn node(wallet: &Wallet, chain: u8, coins: &[u64]) -> pecu_mock::MockChain {
    node_at(wallet, chain, coins, pecu_mock::MockState::default().tip)
}

fn draft(send_all: bool) -> pecu_protocol::SendDraft {
    pecu_protocol::SendDraft {
        from_label: LABEL.to_string(),
        to: recipient(),
        amount: if send_all {
            String::new()
        } else {
            "1.0".to_string()
        },
        from_pool: pecu_protocol::Pool::Transparent,
        send_all,
    }
}

/// The scenario in full.
///
/// The active node is one the user added. It says VRSCTEST, its chain id agrees,
/// and it offers coins from somewhere else entirely. The shipped endpoint has
/// never heard of a single one of them, and no transaction is built.
#[test]
fn a_node_calling_itself_vrsctest_while_serving_coins_the_testnet_node_has_never_heard_of_cannot_fund_a_send(
) {
    let wallet = wallet();
    // Disjoint in both directions, which is what two chains' unspent outputs
    // for one address always are — and three million blocks apart, which is
    // what two chains' *heights* are. The second half is the one this file used
    // to leave out, and leaving it out is what let the shipped verdict be
    // "the second node is behind, try again later".
    let hostile = Chain::Mock(node_at(&wallet, 0xaa, &[5 * COIN, 5 * COIN], MAINNET_TIP));
    let honest = Chain::Mock(node_at(&wallet, 0xbb, &[3 * COIN], TESTNET_TIP));

    let outcome = send::prepare(
        &hostile,
        Some(&Corroborator {
            chain: &honest,
            url: SHIPPED_URL,
        }),
        &wallet.vault,
        LABEL,
        &draft(false),
        "",
    );

    match outcome {
        Err(send::SendError::Refused(SpendRefused::Uncorroborated { count, secondary })) => {
            assert_eq!(count, 2, "both of the hostile node's coins should be named");
            assert_eq!(secondary, SHIPPED_URL);
        }
        // Named specifically, because the failure mode this replaces is
        // "insufficient funds" — the coins filter down to nothing and the form
        // says the wallet is empty, which sends somebody looking for a missing
        // balance instead of a lying node.
        Err(other) => panic!("the wrong refusal: {other:?}"),
        Ok(_) => panic!("a node serving another chain's coins funded a payment"),
    }
}

/// The filter, read back out of the bytes that were signed.
///
/// One coin in common and one the second node has not reached the block of.
/// What is spent is the shared one, and the transaction's input is that
/// outpoint — not "a transaction was built", which would also be true if the
/// guard had done nothing.
#[test]
fn the_transaction_that_is_signed_spends_only_outpoints_both_nodes_have() {
    let wallet = wallet();
    let tip = pecu_mock::MockState::default().tip;

    // The same first coin on both nodes, and a second one only the primary
    // offers, mined above where the second node has got to. Same txid
    // derivation on both sides is what makes the first one the same coin.
    let shared = AddressUtxo {
        utxo: Utxo {
            txid: txid(0x11, 0),
            vout: 0,
            satoshis: Amount::from_sat(4 * COIN),
            script_pubkey: wallet.script.clone(),
        },
        address: wallet.address.clone(),
        height: tip - 500,
        is_spendable: true,
    };
    let unseen = AddressUtxo {
        utxo: Utxo {
            txid: txid(0x22, 0),
            ..shared.utxo.clone()
        },
        height: tip - 150,
        ..shared.clone()
    };

    let primary = with_utxos(&wallet, vec![shared.clone(), unseen.clone()]);
    // Two hundred blocks behind, which is what makes the coin above lag rather
    // than a disagreement.
    let secondary = with_utxos_at(&wallet, vec![shared.clone()], tip - 200);

    let prepared = send::prepare(
        &Chain::Mock(primary),
        Some(&Corroborator {
            chain: &Chain::Mock(secondary),
            url: SHIPPED_URL,
        }),
        &wallet.vault,
        LABEL,
        &draft(true),
        "",
    )
    .expect("four coins can pay a fee");

    let bytes = hex::decode(prepared.signed.hex()).expect("the builder emits hex");
    let tx = verus_sdk::verus_wire::TxV4::deserialize(&bytes).expect("the builder emits a v4 tx");
    assert_eq!(tx.inputs.len(), 1, "{:#?}", tx.inputs);
    assert_eq!(
        tx.inputs[0].txid_internal,
        shared.utxo.txid.to_internal(),
        "the signed transaction spends an outpoint only one node has heard of",
    );
    assert_ne!(tx.inputs[0].txid_internal, unseen.utxo.txid.to_internal());
    assert_eq!(prepared.withheld.count, 1);
    assert_eq!(prepared.corroborated_by, SHIPPED_URL);
}

/// A node holding exactly these outputs, and nothing else changed.
fn with_utxos(wallet: &Wallet, held: Vec<AddressUtxo>) -> pecu_mock::MockChain {
    with_utxos_at(wallet, held, pecu_mock::MockState::default().tip)
}

/// The same, stopped at `tip`.
///
/// The tip is what separates "has not reached that block yet" from "has, and
/// does not have that coin" — lag from disagreement — so a test about either
/// has to be able to move it.
fn with_utxos_at(wallet: &Wallet, held: Vec<AddressUtxo>, tip: u32) -> pecu_mock::MockChain {
    on_chain(wallet, held, tip, 0)
}

/// The same, on a named chain rather than on the default one.
///
/// `MockChain` derives a block hash from the height, so two stock mocks name the
/// same block at every height — they are on the same chain by construction, and
/// `chain_fork` is how a fixture says otherwise. That matters because
/// corroboration asks both nodes to name the block at a height both claim to have
/// reached, which is the only question in the comparison whose answer neither
/// node chooses; a suite that could not express "these are not those blocks"
/// could not reach it, and every test above would be pinning the height rules
/// alone.
fn on_chain(
    wallet: &Wallet,
    held: Vec<AddressUtxo>,
    tip: u32,
    chain_fork: u32,
) -> pecu_mock::MockChain {
    let mut state = pecu_mock::MockState {
        chain_name: "VRSCTEST".to_string(),
        chain_id: TESTNET_ID.to_string(),
        tip,
        chain_fork,
        ..pecu_mock::MockState::default()
    };
    state.utxos.insert(wallet.address.clone(), held);
    pecu_mock::MockChain::new(state)
}

/// Nothing was sent, measured rather than argued.
///
/// `send::prepare` takes no `Broadcaster` and is incapable of sending, which is
/// a property of the signature. The failure worth guarding against is a future
/// refactor handing the build a broadcaster for some unrelated convenience —
/// and a refused build is exactly where that would be least noticed.
#[test]
fn a_refused_send_reaches_no_broadcaster() {
    let wallet = wallet();
    let hostile = node(&wallet, 0xaa, &[5 * COIN]);
    let honest = node(&wallet, 0xbb, &[3 * COIN]);

    let outcome = send::prepare(
        &Chain::Mock(hostile.clone()),
        Some(&Corroborator {
            chain: &Chain::Mock(honest.clone()),
            url: SHIPPED_URL,
        }),
        &wallet.vault,
        LABEL,
        &draft(false),
        "",
    );

    assert!(outcome.is_err());
    assert_eq!(hostile.broadcast_attempts(), 0);
    assert_eq!(honest.broadcast_attempts(), 0);
}

/// The default install, which has to keep working.
///
/// This build ships exactly one endpoint per chain, so on a fresh wallet the
/// active node is a built-in and there is nothing independent to hold it to.
/// `NodeManager::second_source` answers `Unheld` there, the send is built with
/// no second source, and the review says so by saying nothing — see
/// `pecu_chain::network` for why that admission is worth as much as the code.
#[test]
fn a_send_from_a_built_in_endpoint_still_works_when_no_second_source_exists() {
    let wallet = wallet();
    let shipped = pecu_core::shipped_nodes(&pecu_chain::Network::Testnet, false);
    let manager = pecu_chain::NodeManager::new(shipped, pecu_chain::Network::Testnet);
    assert!(
        manager.active().expect("a node is active").builtin,
        "a fresh wallet does not start on a built-in",
    );
    assert!(
        matches!(
            manager.second_source(),
            pecu_chain::SecondSource::Unheld
        ),
        "a default install found a second source it does not ship",
    );

    let chain = Chain::Mock(node(&wallet, 0xcc, &[5 * COIN]));
    let prepared = send::prepare(&chain, None, &wallet.vault, LABEL, &draft(false), "")
        .expect("five coins can pay one");

    assert!(prepared.corroborated_by.is_empty());
    assert_eq!(prepared.withheld.count, 0);
    let review = send::review(1, &prepared, &wallet.address, Amount::from_sat(5 * COIN), true);
    assert_eq!(
        review.corroboration,
        pecu_protocol::NoteVm::none(),
        "the review claimed a check that never happened",
    );
}

/// A send-all that moves less than the balance on screen owes an explanation.
///
/// The commonest honest disagreement is a second node one block behind, and
/// filtering silently is the wrong answer to it: somebody who pressed "send
/// everything" would see a figure below their balance and no reason for it.
#[test]
fn the_review_says_how_much_was_withheld_when_the_second_source_is_behind() {
    let wallet = wallet();
    let tip = pecu_mock::MockState::default().tip;
    let coin = |index: u8, satoshis: u64, height: u32| AddressUtxo {
        utxo: Utxo {
            txid: txid(0x33, index),
            vout: 0,
            satoshis: Amount::from_sat(satoshis),
            script_pubkey: wallet.script.clone(),
        },
        address: wallet.address.clone(),
        height,
        is_spendable: true,
    };

    // Two coins the second node has, and one mined above the block it has
    // reached.
    let primary = with_utxos(
        &wallet,
        vec![
            coin(0, COIN, tip - 500),
            coin(1, COIN, tip - 500),
            coin(2, COIN, tip - 150),
        ],
    );
    let secondary = with_utxos_at(
        &wallet,
        vec![coin(0, COIN, tip - 500), coin(1, COIN, tip - 500)],
        tip - 200,
    );

    let prepared = send::prepare(
        &Chain::Mock(primary),
        Some(&Corroborator {
            chain: &Chain::Mock(secondary),
            url: SHIPPED_URL,
        }),
        &wallet.vault,
        LABEL,
        &draft(true),
        "",
    )
    .expect("two coins can pay a fee");

    assert_eq!(prepared.withheld.count, 1);
    let review = send::review(1, &prepared, &wallet.address, Amount::from_sat(3 * COIN), true);
    assert_eq!(
        review.corroboration,
        pecu_protocol::NoteVm::with(
            "send-withheld",
            ["1".to_string(), SHIPPED_URL.to_string()]
        ),
        "the review did not say what was held back",
    );
}

/// Silence is not agreement, at the layer that spends money.
///
/// `corroborate::against` proves it returns `Unavailable`; that is one layer
/// short of anything irreversible. Replace the `Unavailable` arm in
/// `send::corroborated_funding` with "agree to everything" and the corroborator's
/// own tests stay green — this is the one that goes red. A filtering public
/// proxy answering `-32601` to `getaddressutxos` is the realistic way a second
/// source goes permanently quiet, and treating that as a pass is what would
/// make the whole guard decorative the first time somebody's node went down.
#[test]
fn a_second_source_that_cannot_answer_refuses_the_send_rather_than_waving_it_through() {
    let wallet = wallet();
    let primary = node(&wallet, 0xaa, &[5 * COIN]);
    let mut mute = pecu_mock::MockState {
        chain_name: "VRSCTEST".to_string(),
        chain_id: TESTNET_ID.to_string(),
        fail_reads: Some("scripted outage".to_string()),
        ..pecu_mock::MockState::default()
    };
    mute.utxos.insert(wallet.address.clone(), Vec::new());
    let mute = pecu_mock::MockChain::new(mute);

    let outcome = send::prepare(
        &Chain::Mock(primary.clone()),
        Some(&Corroborator {
            chain: &Chain::Mock(mute),
            url: SHIPPED_URL,
        }),
        &wallet.vault,
        LABEL,
        &draft(false),
        "",
    );

    match outcome {
        // Names the endpoint that went quiet, not the one being checked: a
        // second node exists and is configured here, so a sentence telling
        // somebody to add one would send them after a problem they do not have.
        Err(send::SendError::Refused(SpendRefused::SecondSourceSilent { secondary })) => {
            assert_eq!(secondary, SHIPPED_URL);
        }
        Err(other) => panic!("the wrong refusal: {other:?}"),
        Ok(_) => panic!("a second source that could not answer was read as agreement"),
    }
    assert_eq!(primary.broadcast_attempts(), 0);
}

/// An honest node one block behind is not accused of the attack.
///
/// The commonest wallet shape there is — one coin at the address — against a
/// second node that has not reached its block. Deciding the verdict on "did
/// anything survive the filter" would send this pair the sentence written for a
/// node serving another chain, which contains an accusation, no remedy, and no
/// way to proceed. It gets its own refusal, with the tip in it, because "wait"
/// is only actionable if somebody can see how far behind it is.
#[test]
fn a_single_coin_address_against_a_lagging_node_is_told_to_wait_and_not_accused() {
    let wallet = wallet();
    let tip = pecu_mock::MockState::default().tip;
    let fresh = AddressUtxo {
        utxo: Utxo {
            txid: txid(0x44, 0),
            vout: 0,
            satoshis: Amount::from_sat(5 * COIN),
            script_pubkey: wallet.script.clone(),
        },
        address: wallet.address.clone(),
        height: tip - 150,
        is_spendable: true,
    };
    let primary = with_utxos(&wallet, vec![fresh]);
    let secondary = with_utxos_at(&wallet, Vec::new(), tip - 200);

    let outcome = send::prepare(
        &Chain::Mock(primary),
        Some(&Corroborator {
            chain: &Chain::Mock(secondary),
            url: SHIPPED_URL,
        }),
        &wallet.vault,
        LABEL,
        &draft(false),
        "",
    );

    match outcome {
        Err(send::SendError::Refused(SpendRefused::SecondSourceBehind {
            count,
            secondary,
            tip: reached,
        })) => {
            assert_eq!(count, 1);
            assert_eq!(secondary, SHIPPED_URL);
            assert_eq!(reached, tip - 200);
        }
        // Specifically not `Uncorroborated`, and specifically not "not enough
        // spendable coins" — the two sentences this refusal exists to keep an
        // honest pair away from.
        Err(other) => panic!("an honest lagging node produced: {other:?}"),
        Ok(_) => panic!("a coin no second node has seen was spent"),
    }
}

/// A hostile node that mixes one real coin in with invented ones is still
/// refused.
///
/// A verdict decided by "did anything survive" would let it through as a
/// filtered payment with a tertiary caption about lag — the same sentence, the
/// same tone, as a node one block behind. The heights are what tell them apart:
/// these coins claim to be in blocks the shipped endpoint indexed long ago.
#[test]
fn one_genuine_coin_does_not_buy_a_hostile_node_a_filtered_payment() {
    let wallet = wallet();
    let tip = pecu_mock::MockState::default().tip;
    let coin = |chain: u8, index: u8| AddressUtxo {
        utxo: Utxo {
            txid: txid(chain, index),
            vout: 0,
            satoshis: Amount::from_sat(5 * COIN),
            script_pubkey: wallet.script.clone(),
        },
        address: wallet.address.clone(),
        height: tip - 500,
        is_spendable: true,
    };

    let real = coin(0x55, 0);
    let primary = with_utxos(
        &wallet,
        vec![real.clone(), coin(0x66, 0), coin(0x66, 1), coin(0x66, 2)],
    );
    let secondary = with_utxos(&wallet, vec![real]);

    match send::prepare(
        &Chain::Mock(primary),
        Some(&Corroborator {
            chain: &Chain::Mock(secondary),
            url: SHIPPED_URL,
        }),
        &wallet.vault,
        LABEL,
        &draft(false),
        "",
    ) {
        Err(send::SendError::Refused(SpendRefused::Uncorroborated { count, .. })) => {
            assert_eq!(count, 3, "the invented coins should all be named");
        }
        Err(other) => panic!("a mixed set was downgraded to: {other:?}"),
        Ok(_) => panic!("a node offering three coins nobody else has funded a payment"),
    }
}

/// The same mixture, at the heights it actually arrives with.
///
/// The test above puts the invented coins *below* the shipped endpoint's tip,
/// which is the arrangement where "the second node has indexed that block and
/// does not have the coin" does the work. The scenario in the issue is the
/// other one: a proxy forwarding a mainnet node's answers offers coins three
/// million blocks above anything a VRSCTEST endpoint has reached, and a rule
/// that read every such coin as lag let this exact case through as a filtered
/// payment under a grey caption — spending the one real coin, saying nothing
/// about the rest but a count.
///
/// The genuine coin is at a height the honest node has long since indexed,
/// because it is genuine. The invented ones are where mainnet is.
#[test]
fn one_genuine_coin_does_not_buy_a_hostile_node_a_filtered_payment_from_blocks_nobody_has_reached() {
    let wallet = wallet();
    let coin = |chain: u8, index: u8, height: u32| AddressUtxo {
        utxo: Utxo {
            txid: txid(chain, index),
            vout: 0,
            satoshis: Amount::from_sat(5 * COIN),
            script_pubkey: wallet.script.clone(),
        },
        address: wallet.address.clone(),
        height,
        is_spendable: true,
    };

    let real = coin(0x77, 0, TESTNET_TIP - 500);
    let primary = with_utxos_at(
        &wallet,
        vec![
            real.clone(),
            coin(0x88, 0, MAINNET_TIP - 500),
            coin(0x88, 1, MAINNET_TIP - 400),
            coin(0x88, 2, MAINNET_TIP - 300),
        ],
        MAINNET_TIP,
    );
    let secondary = with_utxos_at(&wallet, vec![real], TESTNET_TIP);

    match send::prepare(
        &Chain::Mock(primary),
        Some(&Corroborator {
            chain: &Chain::Mock(secondary),
            url: SHIPPED_URL,
        }),
        &wallet.vault,
        LABEL,
        &draft(false),
        "",
    ) {
        Err(send::SendError::Refused(SpendRefused::Uncorroborated { count, .. })) => {
            assert_eq!(count, 3, "the invented coins should all be named");
        }
        // Specifically not a `Prepared`. One real coin among three from another
        // chain is enough to pay the 1.0 this draft asks for, so the failure
        // this guards against is a *successful* send, not a different refusal.
        Err(other) => panic!("a mixed set at mainnet heights was downgraded to: {other:?}"),
        Ok(_) => panic!("a hostile node bought a filtered payment with one real coin"),
    }
}

/// The mirror, which must not be an accusation.
///
/// The node being spent through is the one that is behind. It still offers a
/// coin the shipped endpoint has watched be spent — the likeliest cause being
/// an earlier payment from this wallet that confirmed while the node in use was
/// catching up. Read as a disagreement, this pair gets the sentence written for
/// an endpoint serving another chain, over the wallet's own last payment; and
/// the remedy in that sentence, switching to the shipped endpoint, is advice
/// about a node that is working perfectly.
#[test]
fn a_coin_the_shipped_endpoint_has_already_seen_spent_is_not_an_accusation_against_the_node_in_use()
{
    let wallet = wallet();
    let coin = |index: u8, satoshis: u64| AddressUtxo {
        utxo: Utxo {
            txid: txid(0x99, index),
            vout: 0,
            satoshis: Amount::from_sat(satoshis),
            script_pubkey: wallet.script.clone(),
        },
        address: wallet.address.clone(),
        height: TESTNET_TIP - 500,
        is_spendable: true,
    };

    // Two coins on the node in use; the shipped endpoint, two hundred blocks
    // further on, has seen the second one spent.
    let primary = with_utxos_at(&wallet, vec![coin(0, 4 * COIN), coin(1, COIN)], TESTNET_TIP);
    let secondary = with_utxos_at(&wallet, vec![coin(0, 4 * COIN)], TESTNET_TIP + 200);

    let prepared = send::prepare(
        &Chain::Mock(primary),
        Some(&Corroborator {
            chain: &Chain::Mock(secondary),
            url: SHIPPED_URL,
        }),
        &wallet.vault,
        LABEL,
        &draft(true),
        "",
    )
    .expect("the corroborated coin can pay a fee");

    assert_eq!(prepared.withheld.count, 1);
    assert!(
        prepared.withheld.already_spent,
        "the second source is ahead, so this is a spent coin and not an unreached block",
    );
    let review = send::review(1, &prepared, &wallet.address, Amount::from_sat(4 * COIN), true);
    assert_eq!(
        review.corroboration,
        pecu_protocol::NoteVm::with(
            "send-withheld-spent",
            ["1".to_string(), SHIPPED_URL.to_string()]
        ),
        "the review told somebody to wait for a node that is already ahead",
    );
}

/// The route the guard would have missed if it had been written around the
/// word "shielded".
///
/// A `t→z` has no input notes, no witnesses and no anchor. `shield::plan` funds
/// it from `getaddressutxos` at the transparent address — the same set a
/// payment reads — so the same hostile endpoint funds it with the same invented
/// coins, from the same Send form, one character apart in the recipient field.
///
/// The refusal arrives *before* the Sapling parameters are loaded, which is
/// what the deliberately absent paths below assert: reaching them would give
/// `ParamsMissing` instead. Corroboration is the first thing this builder does,
/// because a payment refused before it costs fifty megabytes and thirty seconds
/// of proving is the only kind worth refusing.
#[test]
fn a_shield_is_funded_from_the_same_transparent_coins_and_is_checked_the_same_way() {
    let wallet = wallet();
    // The same two nodes as the transparent scenario, heights included: a
    // shield that was only checked against a pair the field never produces
    // would be pinning the shape of the call rather than the verdict.
    let hostile = Chain::Mock(node_at(&wallet, 0xaa, &[5 * COIN, 5 * COIN], MAINNET_TIP));
    let honest = Chain::Mock(node_at(&wallet, 0xbb, &[3 * COIN], TESTNET_TIP));

    let nowhere = pecu_core::params::Located {
        spend: std::path::PathBuf::from("/nonexistent/sapling-spend.params"),
        output: std::path::PathBuf::from("/nonexistent/sapling-output.params"),
    };
    let draft = pecu_protocol::SendDraft {
        from_label: LABEL.to_string(),
        // The SDK's own `zaddr` test vector, so the destination parses.
        to: "zs18pytujp8qu73a3fu6g9chl7mfumrr0htyqsh60r3ed4capagqwm8tx2l8f9c5g7w87q4566uph3"
            .to_string(),
        amount: "1.0".to_string(),
        from_pool: pecu_protocol::Pool::Transparent,
        send_all: false,
    };

    match send::prepare_shield(
        &hostile,
        Some(&Corroborator {
            chain: &honest,
            url: SHIPPED_URL,
        }),
        &wallet.vault,
        LABEL,
        &wallet.address,
        &draft,
        &nowhere,
    ) {
        Err(send::SendError::Refused(SpendRefused::Uncorroborated { count, secondary })) => {
            assert_eq!(count, 2);
            assert_eq!(secondary, SHIPPED_URL);
        }
        Err(send::SendError::ParamsMissing) => {
            panic!("the shield reached the prover before it checked where the coins came from")
        }
        Err(other) => panic!("the wrong refusal: {other:?}"),
        Ok(_) => panic!("a node serving another chain's coins funded a shield"),
    }
}

/// Issue #45, end to end: #29's scenario with the two tips swapped.
///
/// The wallet is set to the longer chain. The node in use forwards a **shorter**
/// chain's answers — its tip and every coin height with them — so every coin it
/// invents sits *below* the second source's tip. That is rule 3 in
/// `pecu_chain::corroborate`, which excused a missing coin with no bound
/// whenever the second source was the node in front, on the reading that the
/// coin is one this wallet has already spent. The verdict was `OutOfStep` and
/// the payment went out filtered, under a caption saying nothing was wrong.
///
/// One coin in common, which is the half that makes this about money rather than
/// about a sentence: with something left to pay from, `OutOfStep` completes the
/// payment and `Diverged` refuses it. The refusal has to be `Uncorroborated` and
/// not one of the two out-of-step sentences — "wait for the node you are using to
/// catch up" is advice about a node that is on another chain and is never going
/// to arrive.
#[test]
fn a_node_forwarding_a_shorter_chains_answers_is_refused_rather_than_excused() {
    let wallet = wallet();
    let coin = |index: u8, height: u32, satoshis: u64| AddressUtxo {
        utxo: Utxo {
            txid: txid(0x45, index),
            vout: 0,
            satoshis: Amount::from_sat(satoshis),
            script_pubkey: wallet.script.clone(),
        },
        address: wallet.address.clone(),
        height,
        is_spendable: true,
    };

    // The node in use: another chain's blocks, and a tip three million below the
    // one the wallet is set to.
    let primary = on_chain(
        &wallet,
        vec![
            coin(0, TESTNET_TIP - 500, 5 * COIN),
            coin(1, TESTNET_TIP - 499, 5 * COIN),
        ],
        TESTNET_TIP,
        1,
    );
    // The shipped endpoint, on the chain the wallet asked for, holding one of
    // the two.
    let secondary = with_utxos_at(
        &wallet,
        vec![coin(0, TESTNET_TIP - 500, 5 * COIN)],
        MAINNET_TIP,
    );

    let outcome = send::prepare(
        &Chain::Mock(primary),
        Some(&Corroborator {
            chain: &Chain::Mock(secondary),
            url: SHIPPED_URL,
        }),
        &wallet.vault,
        LABEL,
        &draft(false),
        "",
    );

    match outcome {
        Err(send::SendError::Refused(SpendRefused::Uncorroborated { count, secondary })) => {
            assert_eq!(count, 1, "the coin only the node in use has should be named");
            assert_eq!(secondary, SHIPPED_URL);
        }
        // Either of these would be the defect: the payment completing filtered
        // under a caption about two nodes being out of step, or a refusal telling
        // somebody to wait for whichever node it named.
        Err(other) => panic!("a node forwarding a shorter chain produced: {other:?}"),
        Ok(_) => panic!("a node forwarding a shorter chain funded a payment"),
    }
}

/// And the two nodes from that test, put back on one chain, must still pay.
///
/// The same heights, the same coins, the same three-million-block gap — one
/// thing changed, which is that both nodes name the same blocks. This is the
/// honest shape the fix must leave alone: a node in use that is behind, still
/// offering a coin the second source has watched be spent. It pays from what
/// survived and the review says how much was left out.
///
/// Paired with the test above deliberately. A fix that refused a primary for
/// being behind would pass that one and fail this one, and nothing else in this
/// file would have noticed.
#[test]
fn the_same_pair_on_one_chain_still_pays_from_what_the_shipped_endpoint_vouched_for() {
    let wallet = wallet();
    let coin = |index: u8, height: u32, satoshis: u64| AddressUtxo {
        utxo: Utxo {
            txid: txid(0x45, index),
            vout: 0,
            satoshis: Amount::from_sat(satoshis),
            script_pubkey: wallet.script.clone(),
        },
        address: wallet.address.clone(),
        height,
        is_spendable: true,
    };

    let primary = with_utxos_at(
        &wallet,
        vec![
            coin(0, TESTNET_TIP - 500, 5 * COIN),
            coin(1, TESTNET_TIP - 499, 5 * COIN),
        ],
        TESTNET_TIP,
    );
    let secondary = with_utxos_at(
        &wallet,
        vec![coin(0, TESTNET_TIP - 500, 5 * COIN)],
        MAINNET_TIP,
    );

    let prepared = send::prepare(
        &Chain::Mock(primary),
        Some(&Corroborator {
            chain: &Chain::Mock(secondary),
            url: SHIPPED_URL,
        }),
        &wallet.vault,
        LABEL,
        &draft(false),
        "",
    )
    .expect("the corroborated coin can pay");

    assert_eq!(prepared.withheld.count, 1);
    assert!(
        prepared.withheld.already_spent,
        "the second source is ahead, so the coin it will not vouch for is a spent one",
    );
}
