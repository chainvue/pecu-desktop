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
pub mod toast;

/// Everything the binary needs from this crate.
pub mod prelude {
    pub use crate::AppWindow;
    pub use slint::ComponentHandle;
}

/// An identifier written for **speech** rather than for sight.
///
/// A screen reader handed `RQr2cUkF46n7y8WRzDkd1iV9gHusSSQuzX` produces an
/// unbroken run of letters at speaking speed, which nobody can transcribe and
/// nobody can check against anything. In four-character groups it becomes a
/// sequence somebody can write down and compare — which is the entire reason a
/// receive address is on screen at all.
///
/// Sight and speech want opposite things here: on screen the address must be
/// exactly the characters it is, because a space someone copies is an address
/// that fails its checksum. So the grouped form goes only to
/// `accessible-value`, never to `text`.
pub fn spoken(identifier: &str) -> String {
    identifier
        .chars()
        .collect::<Vec<char>>()
        .chunks(4)
        .map(|group| group.iter().collect::<String>())
        .collect::<Vec<String>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    #[test]
    fn an_address_is_grouped_for_speech() {
        assert_eq!(
            super::spoken("RQr2cUkF46n7y8WRzDkd1iV9gHusSSQuzX"),
            "RQr2 cUkF 46n7 y8WR zDkd 1iV9 gHus SSQu zX",
        );
        assert_eq!(super::spoken(""), "");
        assert_eq!(super::spoken("abc"), "abc");
    }
}
