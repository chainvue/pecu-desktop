//! What the vault promises, asserted rather than described.

// Clippy's `allow-expect-in-tests` only covers `#[test]` functions, not the
// free helpers beside them — and this whole file is test code. Panicking on a
// broken fixture is the correct behaviour here: it means the test itself is
// wrong, which should stop the run loudly.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use pecu_keystore::{NewKey, Origin, Vault, VaultError};
use pecu_protocol::Secret;
use verus_sdk::verus_keys::PrivateKey;
use zeroize::Zeroizing;

/// From the SDK's own fixtures, so the address is a known quantity.
const WIF: &str = "UusoQWsobQKUkezgBJa22D9G4t9Avo6k8wD5UUxmmfAEoTN8bawc";
const ADDRESS: &str = "RQr2cUkF46n7y8WRzDkd1iV9gHusSSQuzX";
const PASS: &str = "correct horse battery staple";

fn key() -> PrivateKey {
    PrivateKey::from_wif(WIF).expect("the fixture WIF is valid")
}

fn new_vault(dir: &tempfile::TempDir) -> (Vault, std::path::PathBuf) {
    let path = dir.path().join("vault.json");
    let vault = Vault::create(&path, "test", &Secret::from(PASS)).expect("create");
    (vault, path)
}

#[test]
fn a_key_round_trips_through_lock_and_unlock() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (vault, path) = new_vault(&dir);

    vault
        .add_key("main", NewKey::FromWif { key: key() })
        .expect("add");

    let address = vault
        .with_key("main", |k| k.address().to_string())
        .expect("with_key");
    assert_eq!(address, ADDRESS);

    // Reopened from disk, it is locked and the key is unreachable...
    let reopened = Vault::open(&path).expect("open");
    assert!(!reopened.is_unlocked());
    assert!(matches!(
        reopened.with_key("main", |_| ()),
        Err(VaultError::Locked)
    ));

    // ...but the public facts are readable while locked, which is what lets a
    // receive address work without a passphrase.
    assert_eq!(reopened.keys().len(), 1);
    assert_eq!(reopened.keys()[0].address, ADDRESS);

    reopened.unlock(&Secret::from(PASS)).expect("unlock");
    assert_eq!(
        reopened
            .with_key("main", |k| k.address().to_string())
            .expect("with_key"),
        ADDRESS
    );

    reopened.lock();
    assert!(!reopened.is_unlocked());
}

#[test]
fn the_wrong_passphrase_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (_vault, path) = new_vault(&dir);

    let reopened = Vault::open(&path).expect("open");
    assert!(matches!(
        reopened.unlock(&Secret::from("not it")),
        Err(VaultError::WrongPassphrase)
    ));
    assert!(!reopened.is_unlocked());
}

/// The property the AAD exists for.
///
/// Editing the address by hand must not produce a key that lies about what it
/// controls — it must fail to decrypt at all.
#[test]
fn editing_the_address_invalidates_the_entry() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (vault, path) = new_vault(&dir);
    vault
        .add_key("main", NewKey::FromWif { key: key() })
        .expect("add");

    let text = std::fs::read_to_string(&path).expect("read");
    let tampered = text.replace(ADDRESS, "RPsQDnaxXgrLjcVBh3SpvCpTabWxAdMdzu");
    assert_ne!(
        text, tampered,
        "the fixture address must appear in the file"
    );
    std::fs::write(&path, tampered).expect("write");

    let reopened = Vault::open(&path).expect("open");
    reopened.unlock(&Secret::from(PASS)).expect("unlock");
    assert!(
        reopened.with_key("main", |_| ()).is_err(),
        "an edited address must not yield a usable key",
    );
}

/// An entry lifted into a different vault must not decrypt there.
#[test]
fn an_entry_cannot_be_moved_between_vaults() {
    let dir = tempfile::tempdir().expect("tempdir");

    let (source, source_path) = new_vault(&dir);
    source
        .add_key("main", NewKey::FromWif { key: key() })
        .expect("add");

    let other_path = dir.path().join("other.json");
    let other = Vault::create(&other_path, "other", &Secret::from(PASS)).expect("create");
    other
        .add_key("main", NewKey::FromWif { key: key() })
        .expect("add");

    // Splice the first vault's entry into the second, keeping the second's own
    // wrapped data key and salt.
    let source_doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&source_path).expect("read")).expect("json");
    let mut other_doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&other_path).expect("read")).expect("json");
    other_doc["keys"] = source_doc["keys"].clone();
    std::fs::write(
        &other_path,
        serde_json::to_string(&other_doc).expect("serialize"),
    )
    .expect("write");

    let reopened = Vault::open(&other_path).expect("open");
    reopened.unlock(&Secret::from(PASS)).expect("unlock");
    assert!(
        reopened.with_key("main", |_| ()).is_err(),
        "an entry from another vault must not decrypt here",
    );
}

/// The file must not contain the key in any readable form.
#[test]
fn no_plaintext_key_material_reaches_the_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (vault, path) = new_vault(&dir);

    let phrase = Zeroizing::new("abandon abandon abandon about".to_string());
    vault
        .add_key(
            "main",
            NewKey::Generated {
                key: key(),
                phrase: phrase.clone(),
            },
        )
        .expect("add");

    let text = std::fs::read_to_string(&path).expect("read");
    assert!(!text.contains(WIF), "the WIF is in the file");
    assert!(!text.contains(&*phrase), "the phrase is in the file");
    assert!(
        !text.contains(&hex::encode(*key().to_bytes())),
        "the raw scalar is in the file",
    );
    // The passphrase never touches the file either.
    assert!(!text.contains(PASS));
    // The address, by contrast, is meant to be there — that is what makes the
    // key list work while locked.
    assert!(text.contains(ADDRESS));
}

#[test]
fn revealing_a_phrase_needs_the_passphrase_again() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (vault, _path) = new_vault(&dir);

    let phrase = Zeroizing::new("abandon abandon abandon about".to_string());
    vault
        .add_key(
            "main",
            NewKey::Generated {
                key: key(),
                phrase: phrase.clone(),
            },
        )
        .expect("add");

    // Unlocked is not enough on its own.
    assert!(matches!(
        vault.reveal_phrase("main", &Secret::from("wrong")),
        Err(VaultError::WrongPassphrase)
    ));

    let revealed = vault
        .reveal_phrase("main", &Secret::from(PASS))
        .expect("reveal");
    assert_eq!(*revealed, *phrase);
}

/// A WIF import never had a phrase, and the vault must say so rather than
/// inventing one.
#[test]
fn a_wif_import_has_no_phrase() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (vault, _path) = new_vault(&dir);

    let reference = vault
        .add_key("main", NewKey::FromWif { key: key() })
        .expect("add");
    assert_eq!(reference.origin, Origin::ImportedWif);

    assert!(vault.reveal_phrase("main", &Secret::from(PASS)).is_err());
}

#[test]
fn labels_are_restricted() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (vault, _path) = new_vault(&dir);

    for bad in ["", "../escape", "sub/dir", "UPPER", "-leading", "a b"] {
        assert!(
            matches!(
                vault.add_key(bad, NewKey::FromWif { key: key() }),
                Err(VaultError::BadLabel(_))
            ),
            "`{bad}` should not be a usable label",
        );
    }

    vault
        .add_key("main-2_ok", NewKey::FromWif { key: key() })
        .expect("a sane label");
}

#[test]
fn a_duplicate_label_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (vault, _path) = new_vault(&dir);

    vault
        .add_key("main", NewKey::FromWif { key: key() })
        .expect("add");
    assert!(matches!(
        vault.add_key("main", NewKey::FromWif { key: key() }),
        Err(VaultError::DuplicateLabel(_))
    ));
}

#[test]
fn an_empty_passphrase_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("vault.json");
    assert!(matches!(
        Vault::create(&path, "test", &Secret::from("")),
        Err(VaultError::EmptyPassphrase)
    ));
}

/// Adding a second key to an already-open wallet should not re-prompt.
#[test]
fn adding_a_key_needs_the_vault_open_but_not_the_passphrase() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (vault, path) = new_vault(&dir);
    vault
        .add_key("one", NewKey::FromWif { key: key() })
        .expect("add");

    let reopened = Vault::open(&path).expect("open");
    assert!(matches!(
        reopened.add_key("two", NewKey::FromWif { key: key() }),
        Err(VaultError::Locked)
    ));

    reopened.unlock(&Secret::from(PASS)).expect("unlock");
    reopened
        .add_key("two", NewKey::FromWif { key: key() })
        .expect("add while unlocked");
    assert_eq!(reopened.keys().len(), 2);
}

#[cfg(unix)]
#[test]
fn the_vault_file_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().expect("tempdir");
    let (_vault, path) = new_vault(&dir);

    let mode = std::fs::metadata(&path)
        .expect("metadata")
        .permissions()
        .mode();
    assert_eq!(mode & 0o077, 0, "group/other can read the vault: {mode:o}");
}

/// The backup flag is a fact about the user, not about the key material — so
/// it has to survive a restart, and it has to be readable while locked, which
/// is what lets the wallet know to keep offering the backup screen.
#[test]
fn the_backup_flag_persists_and_is_readable_while_locked() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (vault, path) = new_vault(&dir);

    vault
        .add_key(
            "main",
            NewKey::Generated {
                key: key(),
                phrase: Zeroizing::new("a phrase".to_string()),
            },
        )
        .expect("add");

    // A generated key starts owing its owner a look at the phrase.
    assert!(!vault.keys()[0].backed_up);

    vault.mark_backed_up("main").expect("mark");
    assert!(vault.keys()[0].backed_up);

    // Twice is not an error — the Done button can be pressed twice.
    vault.mark_backed_up("main").expect("mark again");

    let reopened = Vault::open(&path).expect("open");
    assert!(!reopened.is_unlocked());
    assert!(reopened.keys()[0].backed_up);

    // The key itself still decrypts: the flag is outside the AAD on purpose,
    // so writing it cannot have invalidated anything sealed.
    reopened.unlock(&Secret::from(PASS)).expect("unlock");
    assert_eq!(
        reopened
            .with_key("main", |k| k.address().to_string())
            .expect("with_key"),
        ADDRESS,
    );

    assert!(matches!(
        reopened.mark_backed_up("nope"),
        Err(VaultError::NoSuchKey(_))
    ));
}

/// Nothing the user typed in themselves gets a "write this down" prompt.
#[test]
fn imported_keys_start_backed_up() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (vault, _path) = new_vault(&dir);

    vault
        .add_key("wif", NewKey::FromWif { key: key() })
        .expect("add wif");
    vault
        .add_key(
            "phrase",
            NewKey::FromPhrase {
                key: key(),
                phrase: Zeroizing::new("a phrase".to_string()),
            },
        )
        .expect("add phrase");

    for entry in vault.keys() {
        assert!(entry.backed_up, "{} should not be nagged", entry.label);
        assert_ne!(entry.origin, Origin::Generated);
    }
}

/// Changing the passphrase must leave every key exactly where it was.
#[test]
fn a_passphrase_change_keeps_every_key() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (vault, path) = new_vault(&dir);

    vault
        .add_key("one", NewKey::FromWif { key: key() })
        .expect("add one");
    vault
        .add_key(
            "two",
            NewKey::Generated {
                key: PrivateKey::from_bytes(&[7u8; 32], true).expect("a fixed scalar"),
                phrase: Zeroizing::new("a phrase".to_string()),
            },
        )
        .expect("add two");

    let before: Vec<String> = vault.keys().into_iter().map(|k| k.address).collect();
    let salt_before = std::fs::read_to_string(&path).expect("read");

    vault
        .change_passphrase(&Secret::from(PASS), &Secret::from("a different passphrase"))
        .expect("change");

    // Still unlocked: the data key never changed, only its wrapper.
    assert!(vault.is_unlocked());
    assert_eq!(
        vault.with_key("one", |k| k.address().to_string()).unwrap(),
        ADDRESS,
    );

    // The salt is fresh, so the old and new wrappers cannot be attacked
    // together with one derivation.
    let salt_after = std::fs::read_to_string(&path).expect("read");
    assert_ne!(salt_before, salt_after);

    // And from disk, only the new passphrase opens it.
    let reopened = Vault::open(&path).expect("open");
    assert!(matches!(
        reopened.unlock(&Secret::from(PASS)),
        Err(VaultError::WrongPassphrase)
    ));
    reopened
        .unlock(&Secret::from("a different passphrase"))
        .expect("the new passphrase opens it");

    let after: Vec<String> = reopened.keys().into_iter().map(|k| k.address).collect();
    assert_eq!(before, after, "a key moved during a passphrase change");
    assert_eq!(
        reopened
            .with_key("two", |k| k.address().to_string())
            .unwrap(),
        before[1],
    );

    // The phrase is still reachable, under the new passphrase.
    reopened
        .reveal_phrase("two", &Secret::from("a different passphrase"))
        .expect("the phrase survives");
}

/// The wrong old passphrase must change nothing at all.
#[test]
fn a_failed_passphrase_change_leaves_the_vault_alone() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (vault, path) = new_vault(&dir);
    vault
        .add_key("main", NewKey::FromWif { key: key() })
        .expect("add");

    let before = std::fs::read_to_string(&path).expect("read");

    assert!(matches!(
        vault.change_passphrase(&Secret::from("not the passphrase"), &Secret::from("new")),
        Err(VaultError::WrongPassphrase)
    ));
    assert!(matches!(
        vault.change_passphrase(&Secret::from(PASS), &Secret::from("")),
        Err(VaultError::EmptyPassphrase)
    ));

    assert_eq!(std::fs::read_to_string(&path).expect("read"), before);
    Vault::open(&path)
        .expect("open")
        .unlock(&Secret::from(PASS))
        .expect("the original passphrase still works");
}

/// The label is inside the associated data of both sealed blobs, so a rename
/// is a re-seal. What has to survive it is everything: the same key, the same
/// address, and the same recovery phrase — from disk, after a fresh unlock.
#[test]
fn a_renamed_key_is_still_the_same_key() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (vault, path) = new_vault(&dir);

    let phrase = "abandon abandon abandon abandon abandon abandon \
                  abandon abandon abandon abandon abandon about";
    vault
        .add_key(
            "main",
            NewKey::FromPhrase {
                key: key(),
                phrase: Zeroizing::new(phrase.to_string()),
            },
        )
        .expect("add");

    vault.rename_key("main", "savings").expect("rename");

    // The old name is gone and the new one is there, with everything else
    // about the key unchanged.
    let keys = vault.keys();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].label, "savings");
    assert_eq!(keys[0].address, ADDRESS);
    assert_eq!(keys[0].origin, Origin::ImportedPhrase);

    // And from disk, under a fresh unlock — which is the only test that proves
    // the re-seal actually landed rather than the in-memory copy being right.
    let reopened = Vault::open(&path).expect("open");
    reopened.unlock(&Secret::from(PASS)).expect("unlock");

    assert_eq!(
        reopened
            .with_key("savings", |k| k.address().to_string())
            .expect("the key still decrypts under its new name"),
        ADDRESS,
    );
    assert_eq!(
        reopened
            .reveal_phrase("savings", &Secret::from(PASS))
            .expect("the phrase still decrypts under its new name")
            .as_str(),
        phrase,
    );

    assert!(matches!(
        reopened.with_key("main", |_| ()),
        Err(VaultError::NoSuchKey(_)),
    ));
}

/// Two keys under one name would make `with_key` ambiguous, and the label is
/// what every command names a key by.
#[test]
fn a_rename_onto_an_existing_name_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (vault, _path) = new_vault(&dir);

    vault
        .add_key("main", NewKey::FromWif { key: key() })
        .expect("add");
    // A second, different key. Derived from a fixed scalar rather than typed,
    // so it is certainly valid and certainly not the one above.
    vault
        .add_key(
            "spare",
            NewKey::FromWif {
                key: PrivateKey::from_bytes(&[7u8; 32], true).expect("a second key"),
            },
        )
        .expect("add");

    assert!(matches!(
        vault.rename_key("spare", "main"),
        Err(VaultError::DuplicateLabel(_)),
    ));

    // And nothing moved: both keys are still reachable under their own names.
    assert_eq!(
        vault
            .with_key("main", |k| k.address().to_string())
            .expect("main"),
        ADDRESS,
    );
    assert!(vault.with_key("spare", |k| k.address().to_string()).is_ok());
}

/// A label is an identifier the whole application names a key by, so the same
/// rules apply on the way in and on the way through.
#[test]
fn a_rename_to_an_impossible_name_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (vault, _path) = new_vault(&dir);

    vault
        .add_key("main", NewKey::FromWif { key: key() })
        .expect("add");

    for bad in ["", "../escape", "Main", "with space", "-leading"] {
        assert!(
            matches!(vault.rename_key("main", bad), Err(VaultError::BadLabel(_))),
            "`{bad}` was accepted as a label",
        );
    }

    // Still there, under the name it started with.
    assert!(vault.with_key("main", |_| ()).is_ok());
}

/// Renaming needs the vault open — it re-seals under the data key — but not the
/// passphrase. Asking for a passphrase where nothing is revealed trains people
/// to type it at any box that asks.
#[test]
fn renaming_needs_an_unlocked_vault() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (vault, path) = new_vault(&dir);
    vault
        .add_key("main", NewKey::FromWif { key: key() })
        .expect("add");

    let locked = Vault::open(&path).expect("open");
    assert!(matches!(
        locked.rename_key("main", "savings"),
        Err(VaultError::Locked),
    ));
}
