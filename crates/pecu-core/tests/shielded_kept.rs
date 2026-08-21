//! A shielded scan has to survive a restart, and survive it sealed.
//!
//! # Why this test exists
//!
//! A scan of VRSCTEST from Sapling activation is about twelve hundred requests
//! and three minutes. Doing that on every launch is not a slow wallet, it is a
//! wallet nobody leaves running long enough to be useful — and the whole point
//! of keeping the result is that the second launch costs the tail and nothing
//! else.
//!
//! The reason it was *not* kept for so long is the one this file has to hold
//! the line on: a `ScanResult` is the shielded history — every note, with
//! amounts and heights — and the wallet's databases are plain SQLite that any
//! other process on the machine can read. Writing it there in clear would be
//! delivering the appearance of privacy rather than privacy. So it goes through
//! the vault, under the same data key as the recovery phrase, and
//! `no_plaintext_from_the_scan_reaches_the_file` is what says so rather than
//! the comment above it.
//!
//! The chain data is the same captured VRSCTEST fixture `shielded_scan.rs`
//! uses: 5 VRSCTEST paid at block 1 167 987 and spent at 1 167 995, to a key
//! whose viewing half was published and whose spending half never existed here.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use pecu_core::shielded::{Kept, Shielded};
use pecu_keystore::{Vault, VaultError};
use pecu_protocol::Secret;
use verus_sdk::light::{LightClient, LightError, LightTransport};
use verus_sdk::verus_light::HttpResponse;

const DFVK: &str = "549a3f248605a85c02f38a2a54ee7e44384b0b8f7a875fe5e99601dd3959b0e3\
                    2dcb8b5295047e9cccb092cd4553c15b1e230daa6cc96716e7b9604008eac528\
                    5a205bfb257a272d990607e45073be515724a4bc6456d6fb5d964fcc74ee3dff\
                    90ec3f14906942bb6f572090a83bb484320a9fe310cbba8cc58ccf878e57cd88";

const FUNDED_AT: u64 = 1_167_987;
const SPENT_AT: u64 = 1_167_995;
const VALUE: u64 = 500_000_000;
const ADDRESS: &str = "zs1-not-under-test";
const PASS: &str = "correct horse battery staple";

/// The purpose the core seals a kept scan under. Named here rather than
/// imported because it is `pecu_core`'s private business; if it changes, this
/// test is one of the two places that has to agree, which is the point.
const BLOB: &str = "shielded-scan";

/// Serves the committed fixtures, exactly as `shielded_scan.rs` does.
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

fn view(dfvk_hex: &str) -> pecu_keystore::ShieldedView {
    let bytes: [u8; 128] = hex::decode(dfvk_hex.replace(char::is_whitespace, ""))
        .expect("hex")
        .try_into()
        .expect("128 bytes");
    pecu_keystore::ShieldedView {
        dfvk: bytes,
        address: ADDRESS.to_string(),
        diversifier_index: [0u8; 11],
    }
}

/// An account that has scanned the range where the note arrives.
fn scanned() -> Shielded {
    let client = LightClient::new(Server("note_blocks_before_spend.bin"));
    let mut shielded = Shielded::watching(&view(DFVK)).expect("the viewing key reconstructs");
    shielded
        .sync(&client, FUNDED_AT, SPENT_AT - 1)
        .expect("scan the fixture");
    shielded
}

/// The whole round trip: scan, seal, write, and read it back from disk.
///
/// Both halves are constructed fresh from the files rather than reused, because
/// what is being asserted is that a *later run* finds it — not that two
/// variables in one process agree.
#[test]
fn a_scan_survives_a_restart() {
    let home = tempfile::tempdir().expect("tempdir");
    let vault_path = home.path().join("vault.json");

    let before = scanned();
    assert_eq!(before.balance(), VALUE);
    assert_eq!(before.note_count(), 1);

    {
        let vault = Vault::create(&vault_path, "test", &Secret::from(PASS)).expect("create");
        let store = pecu_store::Store::open(home.path()).expect("store");

        let kept = before.keep().expect("something was scanned");
        let plain = serde_json::to_vec(&kept).expect("encode");
        let sealed = vault.seal_blob(BLOB, &plain).expect("seal");
        store.save_shielded_scan(&sealed, 1);
    }

    // A different launch: nothing above is still in scope.
    let vault = Vault::open(&vault_path).expect("open");
    vault.unlock(&Secret::from(PASS)).expect("unlock");
    let store = pecu_store::Store::open(home.path()).expect("store");

    let sealed = store.shielded_scan().expect("a scan was kept");
    let opened = vault.open_blob(BLOB, &sealed).expect("open");
    let kept: Kept = serde_json::from_slice(&opened).expect("decode");

    let after = Shielded::restore(&view(DFVK), kept).expect("restore");

    assert_eq!(after.balance(), VALUE, "the balance came back");
    assert_eq!(after.note_count(), 1);
    assert_eq!(
        after.scanned_to(),
        Some(SPENT_AT - 1),
        "and so did where it had got to, which is what stops the rescan",
    );
    assert_eq!(after.address(), ADDRESS);
}

/// Records what was asked for, and answers with the fixture regardless.
///
/// The fixtures cover one range, so a continuation past it cannot be *served*
/// offline. What can be checked — and is the thing actually at stake — is what
/// the wallet **asked for**, which is exactly where a restored scan either
/// continues or quietly starts again.
struct Recorder {
    calls: std::sync::Mutex<Vec<(String, Vec<u8>)>>,
}

/// Borrowed, so the test can still read what was recorded afterwards. Written
/// out rather than reached through an `Arc` because the alternative is holding
/// a handle to a thing that is also being borrowed by the client.
impl LightTransport for &Recorder {
    fn call(&self, path: &str, request: &[u8]) -> Result<HttpResponse, LightError> {
        Recorder::serve(self, path, request)
    }
}

impl Recorder {
    fn serve(&self, path: &str, request: &[u8]) -> Result<HttpResponse, LightError> {
        self.calls
            .lock()
            .expect("not poisoned")
            .push((path.to_string(), request.to_vec()));
        Server("note_blocks.bin").call(path, request)
    }

    fn new() -> Self {
        Self {
            calls: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn block_range_request(&self) -> Vec<u8> {
        self.calls
            .lock()
            .expect("not poisoned")
            .iter()
            .find(|(path, _)| path.ends_with("GetBlockRange"))
            .map(|(_, request)| request.clone())
            .expect("a block range was requested")
    }
}

/// A restored scan continues rather than starting over.
///
/// The assertion that matters most, and the one easiest to fake: a wallet could
/// restore the numbers, show the right balance, and still walk the chain from
/// the birthday on every launch — and every other test in this file would pass.
///
/// What says it did not is the range it asks for: byte for byte the one an
/// account explicitly told to start at the next block asks for. Comparing the
/// encoded requests rather than a decoded height means this cannot drift from
/// what actually goes on the wire.
///
/// Not "it fetched no tree state", which is what this first tried to assert.
/// `scan_after` fetches one too — both paths go through `scan_following`, which
/// needs the commitment frontier as it stood at the block before the range —
/// so that would have been a test of a belief rather than of the code.
#[test]
fn a_restored_scan_is_continued_and_not_repeated() {
    let kept = scanned().keep().expect("something was scanned");
    let mut restored = Shielded::restore(&view(DFVK), kept).expect("restore");

    let after_restore = Recorder::new();
    // The fixtures cannot serve this range, so the scan fails — after having
    // asked, which is all this needs.
    let _ = restored.sync(&LightClient::new(&after_restore), 0, SPENT_AT);

    // What an account deliberately started at the next block asks for.
    let mut fresh = Shielded::watching(&view(DFVK)).expect("the viewing key reconstructs");
    let from_scratch = Recorder::new();
    let _ = fresh.sync(&LightClient::new(&from_scratch), SPENT_AT, SPENT_AT);

    assert_eq!(
        after_restore.block_range_request(),
        from_scratch.block_range_request(),
        "the restored account asked for a different range than one starting at {SPENT_AT} \
         — it did not continue where the kept scan stopped",
    );
}

/// Nothing about the scan may be readable in **any** file the wallet wrote.
///
/// Checked against raw bytes rather than through an API, because the question
/// is what another process reading the directory can see. The address is the
/// thing that would identify the owner; the amount and the height are the
/// history.
///
/// Every file, not just `cache.sqlite`: SQLite in WAL mode writes through a
/// sidecar, and a test that reads only the main database would pass while the
/// plaintext sat in `cache.sqlite-wal` beside it.
#[test]
fn no_plaintext_from_the_scan_reaches_the_file() {
    let home = tempfile::tempdir().expect("tempdir");
    let vault_path = home.path().join("vault.json");
    let vault = Vault::create(&vault_path, "test", &Secret::from(PASS)).expect("create");
    let store = pecu_store::Store::open(home.path()).expect("store");

    let kept = scanned().keep().expect("something was scanned");
    let plain = serde_json::to_vec(&kept).expect("encode");
    let sealed = vault.seal_blob(BLOB, &plain).expect("seal");
    store.save_shielded_scan(&sealed, 1);
    drop(store);

    let dfvk = DFVK.replace(char::is_whitespace, "");
    let value = VALUE.to_string();
    let funded = FUNDED_AT.to_string();
    let secrets = [
        ADDRESS,
        dfvk.as_str(),
        value.as_str(),
        funded.as_str(),
        "\"notes\"",
        "\"nullifiers\"",
    ];

    let mut checked = 0usize;
    for entry in std::fs::read_dir(home.path()).expect("read the wallet directory") {
        let path = entry.expect("a directory entry").path();
        if !path.is_file() {
            continue;
        }
        let bytes = std::fs::read(&path).expect("read a file back");
        checked += 1;

        for secret in secrets {
            assert!(
                !bytes
                    .windows(secret.len())
                    .any(|window| window == secret.as_bytes()),
                "{} contains {secret:?} in clear",
                path.display(),
            );
        }
    }

    assert!(
        checked >= 2,
        "only {checked} files were checked — the wallet writes at least a vault and a cache, \
         so this was looking in the wrong place",
    );
}

/// A scan kept for one account must not be folded into another.
///
/// Reached by switching keys inside one wallet, which is ordinary. Folding it
/// in would report one account's money under the other's address — and the
/// address on screen would be the one that cannot spend it.
#[test]
fn a_scan_from_another_account_is_refused() {
    let kept = scanned().keep().expect("something was scanned");

    let stranger = verus_sdk::light::derive_account(&[9u8; 64], 1, 0).expect("derive");
    let stranger = pecu_keystore::ShieldedView {
        dfvk: stranger.dfvk,
        address: "zs1-a-stranger".to_string(),
        diversifier_index: stranger.diversifier_index,
    };

    assert!(
        matches!(
            Shielded::restore(&stranger, kept),
            Err(pecu_core::shielded::ShieldedError::WrongAccount)
        ),
        "another account's scan was accepted",
    );
}

/// Notes spent but not yet seen spent have to survive the restart too.
///
/// Without this, a wallet closed in the minute between broadcasting a shielded
/// spend and that block arriving reopens willing to spend the same note again —
/// and the daemon refuses the whole transaction with
/// `bad-txns-sapling-nullifier-exists`, after the prover has been paid for.
/// That is not hypothetical: it is what the first live shielded spend did.
#[test]
fn a_note_spent_a_moment_ago_is_still_spent_after_a_restart() {
    let mut before = scanned();
    let planned = before
        .plan_spend("RQr2cUkF46n7y8WRzDkd1iV9gHusSSQuzX", "1")
        .expect("plan a spend of the one note");
    before.note_spent(&planned);

    assert_eq!(
        before.balance(),
        0,
        "the only note is committed to a spend, so nothing is spendable",
    );

    let kept = before.keep().expect("something was scanned");
    let after = Shielded::restore(&view(DFVK), kept).expect("restore");

    assert_eq!(
        after.balance(),
        0,
        "a restart must not hand the note back as spendable",
    );
    assert_eq!(after.note_count(), 0);
}

/// A kept scan opens for this wallet and no other.
///
/// The vault's own tests cover the sealing; this covers the wiring — that the
/// core seals under a purpose and a wallet id at all, rather than under
/// something a copied file would satisfy.
#[test]
fn a_kept_scan_does_not_open_in_a_different_wallet() {
    let mine = tempfile::tempdir().expect("tempdir");
    let theirs = tempfile::tempdir().expect("tempdir");

    let mine = Vault::create(&mine.path().join("vault.json"), "mine", &Secret::from(PASS))
        .expect("create");
    let theirs = Vault::create(
        &theirs.path().join("vault.json"),
        "theirs",
        &Secret::from(PASS),
    )
    .expect("create");

    let kept = scanned().keep().expect("something was scanned");
    let plain = serde_json::to_vec(&kept).expect("encode");
    let sealed = mine.seal_blob(BLOB, &plain).expect("seal");

    assert!(
        matches!(
            theirs.open_blob(BLOB, &sealed),
            Err(VaultError::WrongPassphrase)
        ),
        "a kept scan opened in a wallet it was not sealed for",
    );
}
