//! What a shielded account is derived from, and what it refuses to be.

// Same reason as `vault.rs`: `allow-expect-in-tests` does not cover the free
// helpers, and a broken fixture must stop the run rather than pass quietly.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use pecu_keystore::{NewKey, Vault, VaultError};
use pecu_protocol::Secret;
use verus_sdk::light::{derive_account, zaddr, COIN_TYPE_MAINNET, COIN_TYPE_TESTNET};
use verus_sdk::verus_keys::{bip39, PrivateKey};
use zeroize::Zeroizing;

/// The BIP-39 all-zero-entropy vector. A real mnemonic with a valid checksum,
/// which "abandon abandon abandon about" — the four-word stand-in the vault
/// tests use — is not.
const MNEMONIC: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon \
                        abandon abandon about";

const WIF: &str = "UusoQWsobQKUkezgBJa22D9G4t9Avo6k8wD5UUxmmfAEoTN8bawc";
const PASS: &str = "correct horse battery staple";

fn key() -> PrivateKey {
    PrivateKey::from_wif(WIF).expect("the fixture WIF is valid")
}

fn vault_with_phrase(dir: &tempfile::TempDir, phrase: &str) -> Vault {
    let vault =
        Vault::create(&dir.path().join("vault.json"), "test", &Secret::from(PASS)).expect("create");
    vault
        .add_key(
            "main",
            NewKey::Generated {
                key: key(),
                phrase: Zeroizing::new(phrase.to_string()),
            },
        )
        .expect("add");
    vault
}

/// The derivation is the composition the module documents, and this asserts it
/// **without** naming an address.
///
/// A test that pinned a literal `zs1…` here would pass just as happily if the
/// coin type were wrong, because the expected value would have come from the
/// same wrong code. So the pieces are walked separately — BIP-39 seed, then
/// `m/32'/133'/0'`, then bech32 — and the result compared. Each of those steps
/// is anchored to an external vector inside the SDK; what this checks is that
/// the wallet joins them in that order and with those constants.
#[test]
fn the_account_is_the_documented_path_and_nothing_else() {
    let seed = bip39::mnemonic_to_seed(MNEMONIC, "").expect("the fixture is a valid mnemonic");
    let expected = derive_account(seed.as_ref(), COIN_TYPE_MAINNET, 0).expect("derive");
    let expected_address = zaddr::encode(&expected.address).expect("encode");

    let dir = tempfile::tempdir().expect("tempdir");
    let view = vault_with_phrase(&dir, MNEMONIC)
        .shielded_view("main")
        .expect("derive the shielded view");

    assert_eq!(view.address, expected_address);
    assert_eq!(view.dfvk, expected.dfvk);
    assert_eq!(view.diversifier_index, expected.diversifier_index);
}

/// Coin type 133 on every network, which is the Verus Mobile path.
///
/// The wallet takes no network parameter, so the only way this can regress is
/// if somebody adds one. This states the consequence of that being wrong: the
/// account would be one no other Verus wallet reaches from the same words.
#[test]
fn the_testnet_coin_type_is_deliberately_not_used() {
    let seed = bip39::mnemonic_to_seed(MNEMONIC, "").expect("mnemonic");
    let zip32_testnet = derive_account(seed.as_ref(), COIN_TYPE_TESTNET, 0).expect("derive");

    let dir = tempfile::tempdir().expect("tempdir");
    let view = vault_with_phrase(&dir, MNEMONIC)
        .shielded_view("main")
        .expect("view");

    assert_ne!(
        view.dfvk, zip32_testnet.dfvk,
        "the wallet derived the stock testnet path; Verus Mobile uses 133 on both networks and \
         would show a different, empty account for these words",
    );
}

/// Verus emits the same human-readable part on both chains, so the address
/// itself does not say which network it belongs to.
#[test]
fn the_address_is_a_zs_address() {
    let dir = tempfile::tempdir().expect("tempdir");
    let view = vault_with_phrase(&dir, MNEMONIC)
        .shielded_view("main")
        .expect("view");

    assert!(
        view.address.starts_with("zs1"),
        "not a Sapling payment address: {}",
        view.address,
    );
    // Round-trips through the decoder the send path would use.
    assert!(zaddr::decode(&view.address).is_ok());
}

/// The spending key is the one the account derives, and it goes no further.
///
/// Checked against `derive_account` directly rather than against a literal, for
/// the same reason the address test is: a pinned hex string would agree with
/// whatever the code produced, including the wrong thing.
#[test]
fn the_spending_key_is_the_accounts_and_stays_in_the_closure() {
    let seed = bip39::mnemonic_to_seed(MNEMONIC, "").expect("mnemonic");
    let expected = derive_account(seed.as_ref(), COIN_TYPE_MAINNET, 0).expect("derive");

    let dir = tempfile::tempdir().expect("tempdir");
    let vault = vault_with_phrase(&dir, MNEMONIC);

    // Only the length escapes. Copying the key out of the closure is exactly
    // what the shape exists to prevent, so the test does not do it either.
    let length = vault
        .with_shielded_key("main", |extsk| {
            assert_eq!(extsk, &*expected.extsk, "not the account's spending key");
            extsk.len()
        })
        .expect("the spending key is reachable while unlocked");

    assert_eq!(length, 169, "a ZIP-32 extended spending key is 169 bytes");
}

/// The spending key needs the wallet open, exactly as the viewing key does.
#[test]
fn a_locked_vault_yields_no_spending_key() {
    let dir = tempfile::tempdir().expect("tempdir");
    let vault = vault_with_phrase(&dir, MNEMONIC);
    vault.lock();

    assert!(matches!(
        vault.with_shielded_key("main", |_| ()),
        Err(VaultError::Locked),
    ));
}

/// And a WIF import has no spending key either — same reason, same wording.
#[test]
fn a_wif_import_has_no_shielded_spending_key() {
    let dir = tempfile::tempdir().expect("tempdir");
    let vault =
        Vault::create(&dir.path().join("vault.json"), "test", &Secret::from(PASS)).expect("create");
    vault
        .add_key("main", NewKey::FromWif { key: key() })
        .expect("add");

    assert!(matches!(
        vault.with_shielded_key("main", |_| ()),
        Err(VaultError::NoPhrase(label)) if label == "main",
    ));
}

/// A WIF import can never have a shielded side, and must say so specifically.
///
/// The interface has to tell somebody this at the moment they ask for a
/// z-address. Reporting a missing key — for a key sitting in front of them in
/// the list — would send them looking for a fault that is not there.
#[test]
fn a_wif_import_has_no_shielded_address() {
    let dir = tempfile::tempdir().expect("tempdir");
    let vault =
        Vault::create(&dir.path().join("vault.json"), "test", &Secret::from(PASS)).expect("create");
    vault
        .add_key("main", NewKey::FromWif { key: key() })
        .expect("add");

    assert!(
        matches!(vault.shielded_view("main"), Err(VaultError::NoPhrase(label)) if label == "main"),
        "a WIF import must be refused as having no phrase, not as a missing key",
    );
}

/// A phrase that is not BIP-39 is a real case, not a corrupt vault.
///
/// The transparent path hashes free text and checks no wordlist, so a phrase
/// restored from a transparent-only wallet can be perfectly valid there and not
/// a mnemonic at all. That must read as "no shielded account", never as damage.
#[test]
fn a_phrase_that_is_not_bip39_is_refused_without_claiming_corruption() {
    let dir = tempfile::tempdir().expect("tempdir");
    let vault = vault_with_phrase(&dir, "these words are not a bip39 mnemonic at all");

    match vault.shielded_view("main") {
        Err(VaultError::Shielded(e)) => {
            assert!(e.to_string().contains("BIP-39"), "unhelpful message: {e}");
        }
        Err(other) => panic!("wrong error for a non-mnemonic phrase: {other}"),
        Ok(_) => panic!("a non-mnemonic phrase produced a shielded account"),
    }
}

/// Locked is locked. The viewing key is reachable only while the vault is open.
#[test]
fn a_locked_vault_yields_no_viewing_key() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("vault.json");
    let vault = Vault::create(&path, "test", &Secret::from(PASS)).expect("create");
    vault
        .add_key(
            "main",
            NewKey::Generated {
                key: key(),
                phrase: Zeroizing::new(MNEMONIC.to_string()),
            },
        )
        .expect("add");

    vault.lock();
    assert!(matches!(
        vault.shielded_view("main"),
        Err(VaultError::Locked)
    ));

    // And unlocking is enough — no passphrase is asked for a second time, which
    // is what lets a scan run on a timer.
    vault.unlock(&Secret::from(PASS)).expect("unlock");
    assert!(vault.shielded_view("main").is_ok());
}

/// The viewing key must not be printable by accident.
///
/// It cannot spend, but it discloses every amount and memo this account will
/// ever receive, permanently and irrevocably. The derived `Debug` would have
/// put all 128 bytes into the first log line that happened to contain one.
#[test]
fn debug_withholds_the_viewing_key() {
    let dir = tempfile::tempdir().expect("tempdir");
    let view = vault_with_phrase(&dir, MNEMONIC)
        .shielded_view("main")
        .expect("view");

    let printed = format!("{view:?}");
    assert!(
        !printed.contains(&hex::encode(view.dfvk)),
        "the full viewing key is in the Debug output",
    );
    // The address is public and is what makes the line useful at all.
    assert!(printed.contains(&view.address));
}

/// The phrase does not survive the call in any reachable form.
#[test]
fn the_vault_file_never_gains_the_viewing_key() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("vault.json");
    let vault = Vault::create(&path, "test", &Secret::from(PASS)).expect("create");
    vault
        .add_key(
            "main",
            NewKey::Generated {
                key: key(),
                phrase: Zeroizing::new(MNEMONIC.to_string()),
            },
        )
        .expect("add");

    let view = vault.shielded_view("main").expect("view");

    let text = std::fs::read_to_string(&path).expect("read");
    assert!(
        !text.contains(&hex::encode(view.dfvk)),
        "deriving a view wrote the viewing key to disk",
    );
    assert!(!text.contains(MNEMONIC), "the phrase is in the file");
}
