//! The wallet's side of a shielded scan, against real captured chain data.
//!
//! No network and no keys that can spend. The blocks in `tests/fixtures/`
//! were captured from VRSCTEST by the SDK on 2026-07-29, when it had 5 VRSCTEST
//! shielded to a key it generated and then spent them again:
//!
//! ```text
//! funded  block 1167987   5 VRSCTEST, note position 3176
//! spent   block 1167995
//! ```
//!
//! The viewing key below is **watch-only** — it finds and values those notes
//! and can spend nothing. The spending key was never in either repository, and
//! the address is empty anyway: the note it held is the one these tests watch
//! being spent.
//!
//! What is asserted here is not the cryptography — `verus-flows` proves that
//! against the same bytes. It is that this wallet holds the result correctly:
//! that a spent note stops counting, that a lagging server is not mistaken for
//! a reorg, and that a first scan and a continuation compose.

// `panic!` is the correct answer in the transport double below: a call it has
// no fixture for means the test is asking for something it never recorded, and
// that must stop the run rather than be papered over with an empty response
// that would read as "the server had nothing".
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use pecu_core::shielded::{Destination, Shielded, ShieldedError};
use pecu_keystore::ShieldedView;
use verus_sdk::light::{LightClient, LightError, LightTransport};
use verus_sdk::verus_light::HttpResponse;

/// The watch-only key for the address that received the note. From the SDK's
/// own committed fixtures.
const DFVK: &str = "549a3f248605a85c02f38a2a54ee7e44384b0b8f7a875fe5e99601dd3959b0e3\
                    2dcb8b5295047e9cccb092cd4553c15b1e230daa6cc96716e7b9604008eac528\
                    5a205bfb257a272d990607e45073be515724a4bc6456d6fb5d964fcc74ee3dff\
                    90ec3f14906942bb6f572090a83bb484320a9fe310cbba8cc58ccf878e57cd88";

const FUNDED_AT: u64 = 1_167_987;
const SPENT_AT: u64 = 1_167_995;
/// 5 VRSCTEST, in satoshis.
const VALUE: u64 = 500_000_000;

/// Somewhere to pay. The SDK's `zaddr` vector and its transparent fixture
/// address — both real, neither anybody's.
const TO_SHIELDED: &str =
    "zs18pytujp8qu73a3fu6g9chl7mfumrr0htyqsh60r3ed4capagqwm8tx2l8f9c5g7w87q4566uph3";
const TO_TRANSPARENT: &str = "RQr2cUkF46n7y8WRzDkd1iV9gHusSSQuzX";

/// Serves the committed tree state and whichever block-range fixture is named.
///
/// The double is deliberately honest about nothing else: `verus-light` checks
/// that a range contains exactly the blocks it asked for, so a fixture that
/// does not match the requested range fails rather than quietly shifting every
/// note position after it.
struct Server(&'static str);

impl LightTransport for Server {
    fn call(&self, path: &str, _request: &[u8]) -> Result<HttpResponse, LightError> {
        let base = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/lightwalletd/");
        let name = if path.ends_with("GetTreeState") {
            "note_treestate_before.bin"
        } else if path.ends_with("GetBlockRange") {
            self.0
        } else {
            panic!("unexpected call to {path}")
        };
        Ok(HttpResponse {
            status: None,
            body: std::fs::read(format!("{base}{name}")).expect("fixture is committed"),
        })
    }
}

/// The address is passed straight through by this layer and is not what any of
/// these tests are about, so it is named as the placeholder it is rather than
/// dressed up as a derivation.
fn view(dfvk_hex: &str) -> ShieldedView {
    let bytes: [u8; 128] = hex::decode(dfvk_hex.replace(char::is_whitespace, ""))
        .expect("hex")
        .try_into()
        .expect("128 bytes");
    ShieldedView {
        dfvk: bytes,
        address: "zs1-not-under-test".to_string(),
        diversifier_index: [0u8; 11],
    }
}

fn watching() -> Shielded {
    Shielded::watching(&view(DFVK)).expect("the viewing key reconstructs")
}

/// Before anything has been scanned the answer is zero, and the wallet can tell
/// that apart from a scan that found nothing.
#[test]
fn an_unscanned_account_is_zero_and_says_it_has_scanned_nothing() {
    let shielded = watching();

    assert_eq!(shielded.balance(), 0);
    assert_eq!(shielded.note_count(), 0);
    assert_eq!(
        shielded.scanned_to(),
        None,
        "a wallet that has not looked must not claim to have looked",
    );
    assert_eq!(shielded.address(), "zs1-not-under-test");
}

/// The note is found and valued, exactly as it was on the chain.
#[test]
fn a_received_note_is_found_and_valued() {
    let client = LightClient::new(Server("note_blocks_before_spend.bin"));
    let mut shielded = watching();

    let progress = shielded
        .sync(&client, FUNDED_AT, SPENT_AT - 1)
        .expect("scan");

    assert_eq!(progress.from, FUNDED_AT);
    assert_eq!(progress.to, SPENT_AT - 1);
    assert_eq!(progress.rewound, 0);

    assert_eq!(shielded.balance(), VALUE);
    assert_eq!(shielded.note_count(), 1);
    assert_eq!(shielded.scanned_to(), Some(SPENT_AT - 1));
}

/// Once the nullifier appears, the note stops counting.
///
/// This is the join a wallet gets wrong by reporting detected notes: the note
/// is still detected in this range, and it is worth nothing.
#[test]
fn a_spent_note_stops_counting() {
    let client = LightClient::new(Server("note_blocks.bin"));
    let mut shielded = watching();

    shielded.sync(&client, FUNDED_AT, SPENT_AT).expect("scan");

    assert_eq!(
        shielded.balance(),
        0,
        "the note was spent in this very range and must not be reported as funds",
    );
    assert_eq!(shielded.note_count(), 0);
}

/// A different key sees nothing here.
///
/// Trial decryption is the whole separation between wallets. If this ever
/// returned notes, every balance the wallet showed would be meaningless.
#[test]
fn another_wallet_sees_none_of_it() {
    let stranger = verus_sdk::light::derive_account(&[9u8; 64], 1, 0).expect("derive");
    let mut shielded = Shielded::watching(&ShieldedView {
        dfvk: stranger.dfvk,
        address: "zs1-a-stranger".to_string(),
        diversifier_index: stranger.diversifier_index,
    })
    .expect("viewing key");

    let client = LightClient::new(Server("note_blocks.bin"));
    shielded.sync(&client, FUNDED_AT, SPENT_AT).expect("scan");

    assert_eq!(shielded.balance(), 0);
    assert_eq!(shielded.note_count(), 0);
}

/// A server that has less of the chain is not a server on a different chain.
///
/// The distinction decides what the wallet does: waiting costs nothing, and
/// treating it as a reorg would discard verified notes and rescan for a fork
/// that does not exist. The scanned state must survive the refusal untouched.
#[test]
fn a_lagging_server_is_refused_without_discarding_anything() {
    let client = LightClient::new(Server("note_blocks_before_spend.bin"));
    let mut shielded = watching();
    shielded
        .sync(&client, FUNDED_AT, SPENT_AT - 1)
        .expect("first scan");

    let refused = shielded.sync(&client, FUNDED_AT, FUNDED_AT + 1);

    match refused {
        Err(ShieldedError::ServerBehind { ours, theirs }) => {
            assert_eq!(ours, SPENT_AT - 1);
            assert_eq!(theirs, FUNDED_AT + 1);
        }
        Err(other) => panic!("a lagging server was reported as something else: {other}"),
        Ok(_) => panic!("a server behind the wallet was accepted"),
    }

    // Nothing was thrown away.
    assert_eq!(shielded.balance(), VALUE);
    assert_eq!(shielded.scanned_to(), Some(SPENT_AT - 1));
}

/// **Upstream panics on a malformed viewing key, and this records it.**
///
/// `Shielded::watching` returns a `Result` and maps a refusal to
/// `BadViewingKey`, but 128 bytes that are not a viewing key never reach that
/// path: `dfvk_from_bytes` calls into `sapling-crypto`, which panics —
/// `keys.rs:207`, "RedJubjub permits the set of valid SpendValidatingKeys" —
/// before it can return anything.
///
/// The SDK states the opposite principle in `derive_account`'s own
/// documentation: it "errors rather than panics on a short seed ... which is
/// not acceptable at a library boundary that takes caller input". The same
/// argument applies here and the dependency does not follow it.
///
/// Why this is not a live hazard in this wallet: the only bytes that reach
/// `watching` come from `ShieldedView`, which derived them a moment earlier
/// from a phrase. There is no path from a file, a socket or a text field to
/// here. So this is written down as a `should_panic` — the behaviour is real
/// and would otherwise be discovered by a crash — rather than worked around
/// with a validation pass that would be dead code today.
///
/// The fix belongs upstream. If `dfvk_from_bytes` ever learns to refuse
/// instead, this test fails, which is the correct moment to delete it and
/// assert `BadViewingKey` instead.
#[test]
#[should_panic(expected = "SpendValidatingKey")]
fn a_malformed_viewing_key_panics_upstream_rather_than_being_refused() {
    let _ = Shielded::watching(&ShieldedView {
        dfvk: [0xff; 128],
        address: "zs1-nonsense".to_string(),
        diversifier_index: [0u8; 11],
    });
}

// ── Planning a spend, which needs no spending key ───────────────────────────

/// A wallet that has not scanned cannot say it has nothing.
///
/// The distinction is the whole point: "no funds" is a claim about the chain,
/// and a wallet that has not looked has not earned it. Reporting one for the
/// other would tell somebody their money is gone.
#[test]
fn planning_before_scanning_says_so_rather_than_claiming_empty() {
    let shielded = watching();

    assert!(matches!(
        shielded.plan_spend(TO_SHIELDED, "1"),
        Err(ShieldedError::NothingScanned),
    ));
}

/// A shielded destination is recognised, and costs no transparent output.
#[test]
fn a_spend_to_a_shielded_address_is_planned() {
    let client = LightClient::new(Server("note_blocks_before_spend.bin"));
    let mut shielded = watching();
    shielded
        .sync(&client, FUNDED_AT, SPENT_AT - 1)
        .expect("scan");

    let planned = shielded.plan_spend(TO_SHIELDED, "1").expect("plan");

    assert!(matches!(planned.to, Destination::Shielded(_)));
    assert_eq!(planned.amount.to_sat(), 100_000_000);
    assert_eq!(planned.fee.to_sat(), 10_000, "not the daemon's own floor");
    assert_eq!(planned.note_count(), 1);
    // The note is worth five and the payment is one: value cannot be split at
    // the input, so the whole note is consumed and the rest comes back as a new
    // one. A review screen that hid this would be hiding where the money went.
    assert_eq!(planned.notes_worth().to_sat(), VALUE);
}

/// A transparent destination is recognised too, from the same field.
#[test]
fn a_spend_to_a_transparent_address_is_planned() {
    let client = LightClient::new(Server("note_blocks_before_spend.bin"));
    let mut shielded = watching();
    shielded
        .sync(&client, FUNDED_AT, SPENT_AT - 1)
        .expect("scan");

    let planned = shielded.plan_spend(TO_TRANSPARENT, "1").expect("plan");

    assert!(matches!(planned.to, Destination::Transparent(_)));
    // Same floor: the allowance covers a recipient, a native change and a token
    // change, and this is inside it.
    assert_eq!(planned.fee.to_sat(), 10_000);
}

/// More than the notes hold is refused with both numbers.
///
/// The balance and what is reachable in one payment are different figures, and
/// a refusal that gave only one of them would be unanswerable: somebody looking
/// at a balance they cannot spend needs to be told why, not told again what the
/// balance is.
#[test]
fn spending_more_than_the_notes_hold_names_both_figures() {
    let client = LightClient::new(Server("note_blocks_before_spend.bin"));
    let mut shielded = watching();
    shielded
        .sync(&client, FUNDED_AT, SPENT_AT - 1)
        .expect("scan");

    match shielded.plan_spend(TO_SHIELDED, "50") {
        Err(ShieldedError::NotEnough {
            held,
            needed,
            reachable,
            notes,
        }) => {
            assert_eq!(held, "5");
            assert_eq!(needed, "50.0001");
            assert_eq!(reachable, "5");
            assert_eq!(notes, 1);
        }
        other => panic!("wrong outcome for an unaffordable spend: {other:?}"),
    }
}

/// Neither kind of address is still neither.
#[test]
fn nonsense_is_not_a_destination() {
    assert!(matches!(
        Destination::read("not-an-address"),
        Err(ShieldedError::BadAddress),
    ));
    // And a shielded address that fails its checksum is a typo, not a z-address.
    assert!(matches!(
        Destination::read("zs1nonsense"),
        Err(ShieldedError::BadAddress),
    ));
}

/// Zero is not an amount, in either direction.
#[test]
fn nothing_is_not_a_payment() {
    let client = LightClient::new(Server("note_blocks_before_spend.bin"));
    let mut shielded = watching();
    shielded
        .sync(&client, FUNDED_AT, SPENT_AT - 1)
        .expect("scan");

    assert!(matches!(
        shielded.plan_spend(TO_SHIELDED, "0"),
        Err(ShieldedError::BadAmount),
    ));
}
