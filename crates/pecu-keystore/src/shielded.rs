//! The shielded half of a key: viewing material derived from its phrase.
//!
//! # Why this is in the keystore and not in the core
//!
//! Deriving a shielded account needs the recovery phrase, and the phrase is
//! sealed in the vault. This crate is the only one allowed to open it, so this
//! is the only place the derivation can happen without widening that rule.
//!
//! What leaves here is deliberately *not* a spending key. [`ShieldedView`]
//! carries the diversifiable full viewing key and the address it produces, and
//! nothing else: enough to find every note paid to this account and to value
//! it, and not enough to move a coin. The extended spending key exists for the
//! few microseconds `derive_account` runs and is dropped inside this module.
//!
//! That is a real distinction rather than a decorative one, and it is also not
//! a small permission. A full viewing key reveals **every incoming and outgoing
//! amount and memo for the account, for all time** — it is the thing an auditor
//! is given. It cannot spend. So it may cross into `pecu-core`, which already
//! holds the unlocked vault and could reach the spending key anyway; it must
//! never cross into `pecu-ui`, which is what `tests/dependency_boundary.rs`
//! enforces by keeping this crate out of that graph entirely.
//!
//! # Two schedules from one phrase
//!
//! A Verus phrase produces two unrelated keys. The transparent R-address comes
//! from `sha256(utf8(phrase))` with the Agama clamp — no BIP-39, no BIP-32. The
//! shielded z-address comes from the real BIP-39 → ZIP-32 path this module
//! walks. They share nothing but the words, which is why a key that arrived as
//! a WIF can never have a shielded side: a WIF is a key, and the words that
//! would have produced it never existed.
//!
//! # `coin_type` is 133 on both networks, and that is not a bug
//!
//! ZIP-32 reserves 1 for testnet, and this wallet does not use it — on VRSCTEST
//! either. Verus Mobile derives with 133 everywhere, because its Kotlin bridge
//! calls `deriveSaplingSpendingKey(seed)` with no network argument and the
//! default is mainnet. The SDK confirmed that against a live VRSCTEST wallet:
//! of five candidate paths, only `m/32'/133'/0'` reproduced the address the app
//! shows.
//!
//! So this takes no network parameter. Accepting one would say the answer
//! depends on the network, and it does not — it would only create a way to
//! derive an account Verus Mobile cannot see, which for a recovery phrase is
//! the worst kind of wrong: everything works, the balance is zero, and nothing
//! says why.

use verus_sdk::light::{derive_account, zaddr, COIN_TYPE_MAINNET};
use verus_sdk::verus_keys::bip39;

/// The account index derived. Account 0 is what Verus Mobile shows, and this
/// wallet offers no way to ask for another — a second account would be a second
/// balance nobody was told about.
const ACCOUNT: u32 = 0;

/// Why a phrase did not produce a shielded account.
#[derive(Debug, thiserror::Error)]
pub enum ShieldedError {
    /// The words are not a valid BIP-39 mnemonic.
    ///
    /// Separate from the vault's own errors because it is not a storage
    /// failure: the phrase decrypted perfectly and simply is not a mnemonic.
    /// Reachable in practice — a phrase restored from another wallet may be a
    /// valid transparent seed and an invalid BIP-39 one, since the transparent
    /// path hashes free text and checks no wordlist.
    #[error("this recovery phrase is not a valid BIP-39 mnemonic, so it has no shielded account")]
    NotBip39,

    /// ZIP-32 refused the seed or the path.
    #[error("the shielded account could not be derived: {0}")]
    Derivation(String),

    /// The 43 raw bytes did not encode.
    #[error("the shielded address could not be encoded: {0}")]
    Encoding(String),
}

/// A shielded account, with the spending half left behind.
///
/// # Why the viewing key is bytes rather than the SDK's type
///
/// `DiversifiableFullViewingKey` is reconstructed from these 128 bytes with
/// `dfvk_from_bytes` wherever a scan needs it. Holding the bytes keeps this
/// struct plain data — storable, comparable, and free of a Sapling type in
/// every signature that passes one along.
#[derive(Clone)]
pub struct ShieldedView {
    /// The diversifiable full viewing key, 128 bytes. Scans; cannot spend.
    pub dfvk: [u8; 128],
    /// The default payment address, bech32 as `zs…`.
    ///
    /// The same human-readable part on both networks — Verus does not split it
    /// the way Zcash does, so a `zs` address does not say which chain it is
    /// for and the wallet must not imply that it does.
    pub address: String,
    /// Which diversifier index the default address was found at.
    ///
    /// Recorded because roughly half of all indices yield no valid diversifier,
    /// so "the default address" is the first one that worked rather than
    /// index 0 by definition. A wallet that later offers a second address needs
    /// to know where the first one came from.
    pub diversifier_index: [u8; 11],
}

impl std::fmt::Debug for ShieldedView {
    /// The address only.
    ///
    /// A full viewing key in a log is a permanent, irrevocable disclosure of
    /// every payment this account will ever receive, and the derived `Debug`
    /// would have printed all 128 bytes of it the first time somebody logged a
    /// struct that happened to contain one. `tests/log_hygiene.rs` in the core
    /// greps captured output for key material; this makes it impossible rather
    /// than merely tested for.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShieldedView")
            .field("address", &self.address)
            .field("dfvk", &"<128 bytes, withheld>")
            // Named rather than omitted so this stays a complete picture of the
            // struct: a reader can see that nothing else is being hidden, and
            // clippy's `missing_fields_in_debug` keeps it that way if a field
            // is ever added.
            .field("diversifier_index", &hex::encode(self.diversifier_index))
            .finish()
    }
}

/// Run `f` with the account's **extended spending key**, then drop it.
///
/// # Why this exists and why it is shaped like this
///
/// Building a shielded spend needs the 169-byte extended spending key: it
/// authorises each note and signs the spend. Nothing else on the shielded path
/// does — finding notes, valuing them and receiving at the address all work
/// from [`ShieldedView`], which cannot spend.
///
/// So this is the exception, and it is written as the same closure the
/// transparent path has used since the beginning: the key is derived, handed to
/// one operation, and gone when that operation returns. There is no accessor
/// that yields it, and no struct that holds it, precisely so that "how long does
/// a spending key live" has one answer instead of one per caller.
///
/// # The cost of that shape, stated plainly
///
/// Proving a Sapling spend takes tens of seconds, and it happens **inside**
/// `f`. So a spending key exists for the length of a proof rather than for the
/// length of a signature — much longer than the transparent path's window,
/// while still being one operation rather than one session.
///
/// The alternative was to hand the key out to a worker thread and let the proof
/// run outside the keystore. That is easier to wire and strictly worse: the key
/// would then outlive the call that needed it, with nothing in the type system
/// saying when it stops. A long window that closes is not the same as no window
/// at all.
///
/// # Errors
///
/// The same as [`view_from_phrase`]: a phrase that is not BIP-39 has no
/// shielded account and never will.
pub fn with_spending_key<R>(
    phrase: &str,
    f: impl FnOnce(&[u8; 169]) -> R,
) -> Result<R, ShieldedError> {
    let seed = bip39::mnemonic_to_seed(phrase, "").map_err(|_| ShieldedError::NotBip39)?;
    let account = derive_account(seed.as_ref(), COIN_TYPE_MAINNET, ACCOUNT)
        .map_err(|e| ShieldedError::Derivation(e.to_string()))?;

    // `account.extsk` is `Zeroizing`, so it is wiped when this scope ends —
    // which is the line after `f` returns.
    Ok(f(&account.extsk))
}

/// Derive the shielded account a recovery phrase produces.
///
/// The BIP-39 passphrase is empty. Verus Mobile has no field for one, so a
/// wallet that used a non-empty one here would derive an account no other Verus
/// wallet could reach from the same words.
pub fn view_from_phrase(phrase: &str) -> Result<ShieldedView, ShieldedError> {
    // 2048 rounds of PBKDF2-HMAC-SHA512. Milliseconds, and the reason a caller
    // that scans on a timer should hold the result rather than call this again.
    let seed = bip39::mnemonic_to_seed(phrase, "").map_err(|_| ShieldedError::NotBip39)?;

    let account = derive_account(seed.as_ref(), COIN_TYPE_MAINNET, ACCOUNT)
        .map_err(|e| ShieldedError::Derivation(e.to_string()))?;

    let address =
        zaddr::encode(&account.address).map_err(|e| ShieldedError::Encoding(e.to_string()))?;

    // `account.extsk` — the spending key — is dropped here, at the end of this
    // scope, and is wiped on drop. Nothing below this line can spend.
    Ok(ShieldedView {
        dfvk: account.dfvk,
        address,
        diversifier_index: account.diversifier_index,
    })
}
