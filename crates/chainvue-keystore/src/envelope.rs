//! The on-disk format.

use serde::{Deserialize, Serialize};

/// Bumped when the shape changes in a way an older reader cannot cope with.
/// Reading refuses anything it does not recognise, rather than guessing.
pub const VAULT_VERSION: u32 = 1;

/// Key derivation, recorded per vault so the cost can be raised later without
/// stranding files already written.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Kdf {
    /// Only `argon2id` today. Present so a future algorithm can be added
    /// without every existing file becoming ambiguous.
    pub algorithm: String,
    pub salt: String,
    pub memory_kib: u32,
    pub iterations: u32,
    pub parallelism: u32,
}

/// Something encrypted.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sealed {
    /// Only `xchacha20poly1305`.
    ///
    /// The 24-byte nonce is why: a fresh random nonce per seal has negligible
    /// collision probability with no counter to persist and no state to get
    /// wrong. With the 12-byte variant the analysis has to be redone every time
    /// a new re-seal path appears — passphrase change, rename, backup.
    pub algorithm: String,
    pub nonce: String,
    pub ciphertext: String,
}

/// Where a key came from. Decides whether a recovery phrase can be shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// Generated here from OS entropy. Has a phrase.
    Generated,
    /// Imported as a recovery phrase. Has one.
    ImportedPhrase,
    /// Imported as a WIF. Has no phrase, and the backup screen must say so
    /// rather than offering a button that cannot work.
    ImportedWif,
}

/// One key in the vault.
///
/// Everything except `secret` and `phrase` is public information — which is
/// what lets the key list, and a receive address, work while locked.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyEntry {
    pub label: String,
    pub address: String,
    pub compressed: bool,
    pub created: u64,
    pub origin: Origin,
    /// The 32 private key bytes, under the data key.
    pub secret: Sealed,
    /// The recovery phrase, under the data key.
    ///
    /// Kept because Verus transparent keys are **not** hierarchical: one phrase
    /// maps to exactly one key, so discarding it would make "show recovery
    /// phrase" impossible for a key this wallet generated. `None` for a WIF
    /// import, which never had one.
    pub phrase: Option<Sealed>,

    /// Whether the phrase has been shown and confirmed written down.
    ///
    /// # Deliberately outside the AAD
    ///
    /// Every other public field here is authenticated, so editing it breaks
    /// decryption rather than silently changing what the file claims. Not this
    /// one, and the asymmetry is on purpose: it is a reminder, not a security
    /// control. Under the AAD a single flipped bit — in a backup copy, on a bad
    /// disk — would make the key itself permanently unreadable, trading "the
    /// wallet nags when it shouldn't" for "the funds are gone". Wrong trade.
    ///
    /// `default` so a vault written before this field existed reads back as
    /// *not* backed up, which is the safe direction to be wrong in.
    #[serde(default)]
    pub backed_up: bool,
}

/// The whole file.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VaultDoc {
    pub version: u32,
    /// 16 random bytes, hex. Binds entries to this vault: an entry lifted into
    /// another file fails to decrypt rather than quietly working.
    pub wallet_id: String,
    pub name: String,
    pub created: u64,
    /// A hint for the UI. **Never a security control** — which chain a key is
    /// used on is decided by the node, not by this string.
    pub network_hint: String,
    pub kdf: Kdf,
    /// The data key, sealed under the passphrase-derived key.
    pub wrapped_dek: Sealed,
    pub keys: Vec<KeyEntry>,
}

impl VaultDoc {
    /// Everything public about the vault, authenticated alongside the data key.
    ///
    /// Editing any of it — the version, the KDF cost, the salt — invalidates
    /// the ciphertext instead of silently changing what the file claims about
    /// how it was protected.
    pub(crate) fn dek_aad(&self) -> String {
        format!(
            "chainvue-vault-v{}|{}|argon2id:{}:{}:{}|{}",
            self.version,
            self.wallet_id,
            self.kdf.memory_kib,
            self.kdf.iterations,
            self.kdf.parallelism,
            self.kdf.salt,
        )
    }
}

impl KeyEntry {
    /// Everything public about this key, authenticated alongside its secret.
    ///
    /// `wallet_id` is in here so an entry cannot be moved between vaults, and
    /// `address` is in here so a file edited to claim a different address fails
    /// to decrypt rather than producing a key that lies about what it controls.
    pub(crate) fn aad(&self, wallet_id: &str, purpose: &str) -> String {
        format!(
            "chainvue-key-v1|{purpose}|{wallet_id}|{}|{}|{}|{:?}",
            self.label, self.address, self.compressed, self.origin,
        )
    }
}
