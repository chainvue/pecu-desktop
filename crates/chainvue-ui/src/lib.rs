//! The Slint interface.
//!
//! # This crate cannot hold a key, and that is enforced by the build
//!
//! Its dependency list is `chainvue-protocol` and `slint`. There is no
//! `verus-sdk`, no `chainvue-keystore`, no `chainvue-core` — so Rust will not
//! resolve `PrivateKey`, `Vault` or `RpcClient` here at all. "The UI never owns
//! secrets" is therefore a property of the dependency graph rather than of
//! anyone's discipline, and `tests/dependency_boundary.rs` fails if that
//! changes.
//!
//! Everything crossing the boundary is a plain view model from
//! `chainvue-protocol`.

/// The component tree `build.rs` generates from `ui/app.slint`.
///
/// Wrapped in its own module so the lint exemptions below apply to **generated
/// code only**. `slint-build` emits `unwrap()` throughout its vtable and item
/// plumbing, and the workspace denies `unwrap_used` — allowing it crate-wide
/// would silently disarm that rule for the hand-written UI code beside it,
/// which is where a stray `unwrap` would actually be a bug worth catching.
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::pedantic,
    clippy::all,
    missing_docs
)]
mod generated {
    slint::include_modules!();
}

pub use generated::*;

pub mod chart;
pub mod fixtures;
pub mod qr;
pub mod seed;
pub mod snapshot;

/// Everything the binary needs from this crate.
pub mod prelude {
    pub use crate::AppWindow;
    pub use slint::ComponentHandle;
}
