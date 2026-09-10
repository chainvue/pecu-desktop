//! The contract between the UI and the wallet core.
//!
//! # Why this crate is nearly empty of dependencies
//!
//! `pecu-ui` depends on `slint` and on this crate, and on nothing else that
//! could carry key material. Because nothing here pulls `verus-sdk`, the UI
//! crate **cannot name** `PrivateKey` — not "is discouraged from naming it",
//! cannot: Rust will not resolve a type from a crate that is not in the
//! dependency list.
//!
//! That is the whole security boundary, and it is a property of the dependency
//! graph rather than of anyone's discipline. `pecu-ui/tests/` asserts it by
//! walking `cargo tree`, so widening it fails a test rather than a review.
//!
//! Adding `verus-sdk`, `pecu-keystore`, or anything re-exporting either, to
//! this crate's dependencies would dissolve it silently. Don't.
//!
//! # Shape
//!
//! * [`Command`] — everything the UI can ask for, in one enum.
//! * [`Event`] — everything the core can say, all of it finished view models.
//! * [`Secret`] — the only type allowed to carry typed secret text inward.
//! * [`UiError`] — a failure in both the forms it needs: for a person, and for
//!   a bug report.

pub mod command;
pub mod error;
pub mod event;
pub mod format;
pub mod models;
mod secret;

pub use command::{Command, ImportMaterial, PendingAction, RefreshScope};
pub use error::{Severity, UiAction, UiError};
pub use event::Event;
pub use format::{coins, coins_u64};
pub use models::{
    AssetVm, BalanceVm, ChainChoiceVm, ChainHaltVm, ChartPointVm, ChartVm, ContentEntryVm,
    ContentValueVm, ConvertDraft, ConvertQuoteVm, ConvertReviewVm, CurrencyChoicesVm,
    CurrencyDraft, CurrencyDraftVm, CurrencyFieldVm, CurrencyPickVm, CurrencyProblemVm,
    CurrencySliceVm, CurrencyVm, DraftValidationVm, EligibleIdentityVm, FlowStepVm, HistoryRowVm,
    IdentityDetailVm, IdentityVm, KeyFundsVm, KeyOrigin, KeyVm, KnownAddressVm, LaunchDoneVm,
    LaunchPendingVm, LaunchReviewVm, ListDelta, LockReason, MarketDetailVm, MarketRowVm, NetworkVm,
    NodeVm, NoteVm, PendingVm, Pool, PortfolioVm, PreallocationDraft, Reachability, RegistrationVm,
    ReserveDraft, ReviewOutputVm, Route, ScreenId, SearchHitVm, SeedWordVm, SendDraft,
    SendOutcomeVm, SendReviewVm, ShieldedFunds, SpendGate, StatVm, TaskKind, TxDetailVm,
    TxDirection, VenueVm, WalletVm,
};
pub use secret::Secret;

/// The `verus-rust-sdk` revision this workspace is pinned to.
///
/// Surfaced in Settings → Advanced. The SDK is not on crates.io and sets
/// `rust-version = 1.95` against a machine that has exactly 1.95.0, so which
/// revision is in the build is a fact worth being able to read off a screen
/// rather than out of a lock file.
///
/// Written by hand, and it had gone stale twice by the time anybody looked:
/// the screen said `b849fb95` while the build was on `a08d652d`, which is
/// worse than the screen not existing — somebody reading it to answer "which
/// SDK is this" got a confident wrong answer. `the_sdk_revision_on_screen_is_
/// the_one_in_the_build` reads the workspace manifest and holds the two
/// together, so the next repin that forgets this line fails the test suite
/// rather than shipping.
pub const SDK_REV: &str = "a08d652ddb0837efafa2b5df251ca8d6c28206ae";

/// Satoshis per coin. The SDK's `Amount` counts in satoshis; this is the same
/// constant as `verus_sdk::money::SATS_PER_COIN`, restated here because this
/// crate deliberately does not depend on the SDK.
pub const SATS_PER_COIN: i64 = 100_000_000;

/// The word somebody has to type before a revocation is sent.
///
/// The same shape the spending switch uses, and for the same reason: a
/// revocation cannot be undone without the recovery authority, and an identity
/// that is its own recovery authority cannot be recovered at all. A button that
/// only needs to be clicked is one that gets clicked.
///
/// Here rather than in the core because it is a term of the contract: the core
/// sends it, the interface prompts with it and refuses to enable its button
/// without it, and the core checks what comes back. This crate is the one both
/// halves are allowed to name, so this is the one place it can be written once.
///
/// **The check still happens in the core**, comparing against this constant. A
/// second interface that ignored the prompt entirely would still be refused.
pub const REVOKE_CONFIRMATION: &str = "revoke";

/// The shortest passphrase this wallet will let anybody *choose*.
///
/// # Why there is a floor at all
///
/// The vault runs Argon2id at 64 MiB and 3 passes, which is around 3.4× the
/// OWASP interactive minimum, and that is a genuinely good number. What it buys
/// is time per guess. It does not help when the guess is `cat123`, because an
/// attacker holding the vault file does not need many guesses — and the vault
/// file is the thing that leaves with the laptop. Every other irreversible step
/// in this wallet is gated: the phrase reveal, the word-by-word backup check,
/// the typed word before a mainnet spend. This was the one that took any answer
/// at all.
///
/// # Why twelve, and why it is a floor and not a score
///
/// Twelve is the length below which a passphrase somebody invented at a prompt
/// is reliably inside the space an offline attacker searches, and above which
/// the Argon2 cost starts to mean something. It is not a promise: twelve
/// characters can still be `passwordpass`. That is why the interface states
/// what makes a passphrase good beside this number instead of scoring what was
/// typed. A meter that called `Passw0rd!` strong would be worse than saying
/// nothing, because it would be believed.
///
/// # Where it is enforced
///
/// In `pecu_keystore::Vault`, at the two places a passphrase is *set* —
/// `create` and the new half of `change_passphrase` — beside the
/// already-existing refusal of an empty one. Deliberately **not** on the unlock
/// path, which is `derive`: a wallet sealed before this rule existed has to
/// keep opening. See `VaultError::PassphraseTooShort`.
///
/// Here rather than in the keystore for the same reason as
/// [`REVOKE_CONFIRMATION`] above: the interface has to name the number to say
/// it out loud and to gate its own button, and this crate is the only one both
/// halves are allowed to see. The interface repeating it is a convenience;
/// **the refusal is the vault's**, so a second interface that never showed the
/// hint would still be refused.
pub const MIN_PASSPHRASE_CHARS: usize = 12;

#[cfg(test)]
mod sdk_rev_tests {
    /// The revision on the About screen must be the revision in the build.
    ///
    /// [`super::SDK_REV`] is a hand-written string and had drifted twice before
    /// anything checked it. There is no way to read a git dependency's `rev`
    /// from inside the compiled crate, so this reads the workspace manifest —
    /// the same file Cargo resolved the dependency from — and compares.
    ///
    /// Every `verus-*` line is checked rather than the first: three crates are
    /// pinned separately and a repin that moved two of them would otherwise
    /// pass on the strength of the one it did move.
    #[test]
    fn the_sdk_revision_on_screen_is_the_one_in_the_build() {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../Cargo.toml")
            .canonicalize()
            .expect("the workspace manifest sits two levels above this crate");
        let text = std::fs::read_to_string(&manifest).expect("read the workspace manifest");

        let pins: Vec<(&str, &str)> = text
            .lines()
            .filter(|line| line.starts_with("verus-") && line.contains("rev = \""))
            .map(|line| {
                let name = line.split_whitespace().next().unwrap_or(line);
                let rev = line
                    .split_once("rev = \"")
                    .and_then(|(_, rest)| rest.split_once('"'))
                    .map(|(rev, _)| rev)
                    .expect("a rev = \"…\" this line was selected for");
                (name, rev)
            })
            .collect();

        assert!(
            !pins.is_empty(),
            "no git-pinned verus crate found in {}: this test has stopped \
             checking anything",
            manifest.display()
        );

        for (name, rev) in pins {
            assert_eq!(
                rev,
                super::SDK_REV,
                "{name} is pinned to {rev} but the About screen says {}",
                super::SDK_REV
            );
        }
    }
}
