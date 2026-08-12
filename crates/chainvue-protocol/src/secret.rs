//! The one type allowed to carry user-typed secret text across the UI boundary.

use zeroize::Zeroizing;

/// A passphrase or recovery phrase on its way from the UI into the wallet core.
///
/// # What this type is for
///
/// Three commands genuinely need secret text from a text field: unlocking,
/// changing the passphrase, and importing a key. Everything else the UI sends is
/// an intent. `Secret` exists so those three are *visible* — greppable, and
/// obviously different from an ordinary `String` in a review.
///
/// # What it does, and what it cannot do
///
/// It wipes on drop, refuses to print itself, and cannot be serialized or
/// cloned. That shortens the window in which a passphrase sits in this process's
/// memory.
///
/// It does **not** reach backwards. A password typed into a Slint `TextInput`
/// already exists as a refcounted `SharedString` inside the UI toolkit before
/// this type ever sees it, and nothing here can wipe that copy. The mitigation
/// is at the call site and is part of the contract: read the property, move it
/// into a `Secret`, and assign `""` back to the property **in the same
/// callback**, before returning. See the `Actions` global in `chainvue-ui`.
///
/// That residual is documented rather than papered over, because a type that
/// claimed to guarantee more than it can is worse than one that says where it
/// stops.
///
/// # The two properties that are enforced, not merely intended
///
/// A secret must not be serializable — otherwise it rides along in any
/// `#[derive(Serialize)]` on a containing type, into a log or a cache:
///
/// ```compile_fail
/// # use chainvue_protocol::Secret;
/// fn assert_serialize<T: serde::Serialize>() {}
/// assert_serialize::<Secret>();
/// ```
///
/// And it must not be cloneable, because a second copy is a second thing to
/// wipe and nothing tracks the first:
///
/// ```compile_fail
/// # use chainvue_protocol::Secret;
/// let a = Secret::new("x".to_string());
/// let _b = a.clone();
/// ```
///
/// Both of these are `compile_fail` doc-tests on a public item, so `cargo test`
/// actually runs them. The same assertions written inside a `#[cfg(test)]`
/// module would **not** be collected as doc-tests and would silently prove
/// nothing.
pub struct Secret(Zeroizing<String>);

impl Secret {
    /// Take ownership of secret text.
    pub fn new(text: String) -> Self {
        Self(Zeroizing::new(text))
    }

    /// Borrow the text, for the one call that has to consume it.
    ///
    /// Deliberately not `Deref`: reaching the inner `str` should be a visible
    /// act at the call site, not something that happens by coercion.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Whether anything was typed at all.
    ///
    /// Length is not secret in any threat model that matters here — a UI that
    /// cannot grey out its own submit button is worse.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<String> for Secret {
    fn from(text: String) -> Self {
        Self::new(text)
    }
}

impl From<&str> for Secret {
    fn from(text: &str) -> Self {
        Self::new(text.to_string())
    }
}

/// Deliberately opaque. A secret must not reach a log through a derived `Debug`,
/// and `Command` derives `Debug` for tracing.
impl core::fmt::Debug for Secret {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("<secret>")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_does_not_leak_the_secret() {
        let secret = Secret::new("hunter2".to_string());
        let rendered = format!("{secret:?}");
        assert_eq!(rendered, "<secret>");
        assert!(!rendered.contains("hunter2"));
    }

    /// The `Debug` opacity has to survive being nested inside a derived
    /// `Debug`, because that is how it would actually leak: `Command` derives
    /// `Debug` and gets traced.
    #[test]
    fn debug_stays_opaque_when_nested() {
        #[derive(Debug)]
        #[allow(dead_code)]
        struct Wrapper {
            passphrase: Secret,
        }
        let rendered = format!(
            "{:?}",
            Wrapper {
                passphrase: Secret::new("hunter2".to_string()),
            }
        );
        assert!(
            !rendered.contains("hunter2"),
            "leaked through a derive: {rendered}"
        );
        assert!(rendered.contains("<secret>"));
    }

    #[test]
    fn empty_is_reported_without_exposing_anything() {
        assert!(Secret::new(String::new()).is_empty());
        assert!(!Secret::new("x".to_string()).is_empty());
    }
}
