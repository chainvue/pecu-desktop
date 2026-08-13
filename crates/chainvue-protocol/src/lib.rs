//! The contract between the UI and the wallet core.
//!
//! # Why this crate is nearly empty of dependencies
//!
//! `chainvue-ui` depends on `slint` and on this crate, and on nothing else that
//! could carry key material. Because nothing here pulls `verus-sdk`, the UI
//! crate **cannot name** `PrivateKey` — not "is discouraged from naming it",
//! cannot: Rust will not resolve a type from a crate that is not in the
//! dependency list.
//!
//! That is the whole security boundary, and it is a property of the dependency
//! graph rather than of anyone's discipline. `chainvue-ui/tests/` asserts it by
//! walking `cargo tree`, so widening it fails a test rather than a review.
//!
//! Adding `verus-sdk`, `chainvue-keystore`, or anything re-exporting either, to
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
    AssetVm, BalanceVm, ChartPointVm, ChartVm, DraftValidationVm, HistoryRowVm, KeyOrigin, KeyVm,
    ListDelta, LockReason, NetworkVm, NodeVm, PendingVm, PortfolioVm, Reachability, ReviewOutputVm,
    ScreenId, SeedWordVm, SendDraft, SendOutcomeVm, SendReviewVm, TaskKind, TxDetailVm,
    TxDirection, WalletVm,
};
pub use secret::Secret;

/// The `verus-rust-sdk` revision this workspace is pinned to.
///
/// Surfaced in Settings → Advanced. The SDK is not on crates.io and sets
/// `rust-version = 1.95` against a machine that has exactly 1.95.0, so which
/// revision is in the build is a fact worth being able to read off a screen
/// rather than out of a lock file.
pub const SDK_REV: &str = "b849fb959ee70885327640dc796ba59932834a72";

/// Satoshis per coin. The SDK's `Amount` counts in satoshis; this is the same
/// constant as `verus_sdk::money::SATS_PER_COIN`, restated here because this
/// crate deliberately does not depend on the SDK.
pub const SATS_PER_COIN: i64 = 100_000_000;
