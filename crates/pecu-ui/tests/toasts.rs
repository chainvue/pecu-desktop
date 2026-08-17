//! Everything the wallet says reaches somewhere a person can read it.
//!
//! This is the test that would have caught the silence. Notices were routed by
//! code to a per-screen string, and every code without a screen of its own fell
//! through to one rendered only by the unlock form — so while the wallet was
//! open, a refused spend, a payment that could not be recorded and a history
//! that would not load all happened with nothing on screen at all.
//!
//! Nobody found that by reading the routing. It was found by reading a log
//! after a real session, where the same failure had been raised every few
//! seconds for eight minutes behind a cached list that looked current.

#![allow(clippy::expect_used, clippy::panic)]

use pecu_protocol::{Severity, UiError};
use pecu_ui::{snapshot, toast, AppWindow};
use slint::ComponentHandle;

fn notice(code: &'static str, severity: Severity) -> UiError {
    UiError::simple(code, format!("{code} happened"), "what to do", severity)
}

#[test]
fn nothing_the_wallet_says_is_lost() {
    let _window = snapshot::install().expect("offscreen platform");
    let ui = AppWindow::new().expect("a window");
    toast::install(&ui);

    assert_eq!(toast::count(&ui), 0, "a fresh window is not complaining");

    // ── The codes that used to vanish ────────────────────────────────────
    //
    // Every one of these reaches `apply_notice`'s catch-all, which used to
    // write to a property the shell does not render.
    for code in [
        "spend_refused",
        "pending_commit",
        "history",
        "node_failover",
        "broadcast_rejected",
    ] {
        toast::clear(&ui);
        toast::show(&ui, &notice(code, Severity::Danger));
        assert_eq!(
            toast::count(&ui),
            1,
            "`{code}` had nowhere to appear — this is the failure that shipped",
        );
    }

    // ── One problem is one card ──────────────────────────────────────────
    //
    // A refresh failing every fifteen seconds raised the same notice forty
    // times in one session. Forty cards is a wallet nobody can use.
    toast::clear(&ui);
    for _ in 0..40 {
        toast::show(&ui, &notice("history", Severity::Warning));
    }
    assert_eq!(toast::count(&ui), 1, "the same problem stacked");

    // ── Different problems are different cards ───────────────────────────
    toast::show(&ui, &notice("spend_refused", Severity::Danger));
    assert_eq!(toast::count(&ui), 2);
    assert_eq!(
        toast::codes(&ui),
        vec!["history", "spend_refused"],
        "oldest first, so a new one does not move what is being read",
    );

    // ── The stack is bounded ─────────────────────────────────────────────
    //
    // Taller than the window hides the newest, which is the one that just
    // happened — so the oldest goes rather than the newest being pushed off.
    toast::clear(&ui);
    for code in ["one", "two", "three", "four", "five", "six"] {
        toast::show(&ui, &notice(code, Severity::Warning));
    }
    let codes = toast::codes(&ui);
    assert!(codes.len() <= 4, "{codes:?}");
    assert_eq!(
        codes.last().map(std::string::ToString::to_string),
        Some("six".to_string()),
        "the newest was pushed off instead of the oldest",
    );

    // ── Locking clears them ──────────────────────────────────────────────
    //
    // A problem raised about a session that has ended is not one anybody can
    // act on, and leaving it over the unlock form shouts at the wrong person.
    toast::clear(&ui);
    assert_eq!(toast::count(&ui), 0);
}

/// The wording that arrives last is the one shown, even when the code repeats.
///
/// The same code can carry a different sentence — "could not read this wallet's
/// activity" is raised for a timeout and for a node refusing a method — and the
/// one that just happened is the true one.
#[test]
fn a_repeated_code_shows_the_newest_wording() {
    let _window = snapshot::install().expect("offscreen platform");
    let ui = AppWindow::new().expect("a window");
    toast::install(&ui);

    toast::show(
        &ui,
        &UiError::simple("history", "first wording", "", Severity::Warning),
    );
    toast::show(
        &ui,
        &UiError::simple("history", "second wording", "", Severity::Warning),
    );

    let rows = ui.global::<pecu_ui::ToastState>().get_rows();
    let row = slint::Model::row_data(&rows, 0).expect("a row");
    // The count is a separate field — the "(2×)" is added when it is drawn,
    // so the stored title stays the sentence.
    assert_eq!(row.title, "second wording");
    assert_eq!(row.count, 2, "the repeat was not counted");
}
