//! What the UI is told when something fails.

use serde::Serialize;

use crate::models::NoteVm;

/// How loudly to say it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Severity {
    /// A fact, not a problem. "Spending on mainnet is turned off."
    Info,
    /// Something did not work and probably will later. A node timed out.
    Warning,
    /// Money is involved, or the user must decide something. A rejected
    /// transaction, an ambiguous broadcast.
    Danger,
}

/// A button the error itself offers.
///
/// Errors that a user can do something about should carry the doing with them —
/// a toast that says "check your connection" and offers no way to change the
/// node is a dead end.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum UiAction {
    /// Try the same thing again. Only offered where retrying is *safe* —
    /// never on a broadcast.
    Retry,
    /// Jump to Settings → Network.
    OpenNodeSettings,
    /// Ask the node whether a transaction we are unsure about actually landed.
    CheckPending { pending_id: u64 },
    /// Put the raw signed bytes on the clipboard, so they are not lost with the
    /// process.
    CopyHex { pending_id: u64 },
    /// Copy the full technical text for a bug report.
    CopyDetails,
    /// Acknowledge and move on.
    Dismiss,
}

/// An error, in the two forms it needs to exist in at once.
///
/// `title` and `detail` are for a person. `technical` is the full source chain,
/// for the log file and the "Copy details" button. Both are always present:
/// showing a user `RpcError::Transport("connection reset by peer")` is a failure
/// of the product, and hiding it from a bug report is a failure of the tooling.
/// Only `Serialize`, deliberately: `code` is a `&'static str` because every
/// code is a compile-time constant, and that cannot be deserialized into an
/// arbitrary lifetime. Nothing needs to read one back — these cross an
/// in-process channel and a log file, never a wire.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct UiError {
    /// Stable, greppable identifier. Appears in logs and in bug reports, and
    /// never changes once shipped — it is what a support conversation is about.
    pub code: &'static str,
    /// What happened, as a **named reason** rather than a sentence.
    ///
    /// The core knows what went wrong. It does not know how wide the line is,
    /// what is beside it, or what language it is in — so it names the reason and
    /// supplies the values, and `components/note.slint` has the words. Same
    /// arrangement as every field-level refusal, for the same reasons, and it is
    /// why a catalogue can now reach a toast at all.
    pub message: NoteVm,
    /// What to do about it, when that is something **only the machine can
    /// say** — a node's own error text, or a reason the core assembled from one.
    ///
    /// Not translatable and not meant to be: these are the words a daemon used.
    /// Prose that the *wallet* wrote belongs in the message above, where a
    /// catalogue can reach it.
    pub detail: String,
    /// The whole cause chain, walked through `std::error::Error::source`.
    pub technical: String,
    pub severity: Severity,
    pub actions: Vec<UiAction>,
}

impl UiError {
    /// An error with nothing for the user to do but read it.
    pub fn simple(
        code: &'static str,
        message: NoteVm,
        detail: impl Into<String>,
        severity: Severity,
    ) -> Self {
        Self {
            code,
            message,
            detail: detail.into(),
            technical: String::new(),
            severity,
            actions: vec![UiAction::Dismiss],
        }
    }

    /// Attach the technical cause chain.
    ///
    /// `#[must_use]` because `error.with_technical(e);` as a statement compiles,
    /// does nothing, and loses the cause chain — exactly the bug this method
    /// exists to prevent.
    #[must_use]
    pub fn with_technical(mut self, technical: impl Into<String>) -> Self {
        self.technical = technical.into();
        self
    }

    /// Replace the offered actions.
    #[must_use]
    pub fn with_actions(mut self, actions: Vec<UiAction>) -> Self {
        self.actions = actions;
        self
    }
}
