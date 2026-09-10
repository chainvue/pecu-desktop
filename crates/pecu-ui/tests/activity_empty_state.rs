//! An Activity tab that is empty says which kind of empty it is.
//!
//! # Why this is a test and not only a picture
//!
//! Two reference images hold the wording — `history-empty` and
//! `history-cannot-fill` — and for wording that is the right place. What a
//! picture cannot hold is the *pairing*, and the pairing is the whole of issue
//! #12. Three of the five filters on this screen select a kind
//! `pecu_protocol::HISTORY_KINDS_PRODUCED` says nothing produces, so their
//! lists are empty for a reason that has nothing to do with the wallet — and
//! the screen used to answer all five with one sentence about everything the
//! wallet had ever sent or received.
//!
//! So the assertions here are about which sentence reaches which tab, and
//! about the one word that must never appear on a tab that cannot fill: a
//! count. "No sign-ins yet" is a claim about the world, and this build has
//! never looked. `UPDATE_SNAPSHOTS=1` would accept that sentence back onto the
//! Logins tab without a murmur; this does not.
//!
//! # And why the flag rather than the tab name
//!
//! Nothing below asks whether the tab is Logins. It asks
//! `ActivityState.filter-can-fill`, which is what the screen asks, and sets it
//! through `pecu_protocol::history_filter_can_fill` — the core's own answer.
//! The day the identity work in #9 lands and `login` joins the produced kinds,
//! `a_tab_that_cannot_fill_says_so` stops describing the Logins tab and starts
//! describing whichever tab is still switched off, and
//! `a_tab_that_can_fill_is_not_excused` starts covering Logins instead. Neither
//! of them has to be edited, and neither of them can be left asserting
//! something the interface no longer says.

#![allow(clippy::expect_used, clippy::panic)]

use i_slint_backend_testing::ElementQuery;
use pecu_ui::{ActivityState, AppWindow, WalletState};
use slint::ComponentHandle;
use std::sync::{Mutex, MutexGuard, PoisonError};

/// The sentence the whole list gets, which must not reach a slice of it.
const ABOUT_THE_WHOLE_LIST: &str = "Everything this wallet has ever sent or received";

/// One window on this process at a time.
///
/// The same reason `tests/balance_captions.rs` has one: the testing backend is
/// process-global, and the threads Cargo runs a file's tests as would fight
/// over it. `into_inner` on a poisoned lock so a test that failed holding it
/// does not bury the failure worth reading under lock panics.
static ONE_WINDOW_AT_A_TIME: Mutex<()> = Mutex::new(());

fn alone() -> MutexGuard<'static, ()> {
    ONE_WINDOW_AT_A_TIME
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// The Activity screen on a wallet with no history, on the given tab.
///
/// The filter and the core's answer about it are set together, exactly as
/// `main.rs::wire_history` sets them — a test that set only the filter would be
/// asserting against a state the wallet cannot be in.
fn activity(filter: &str) -> AppWindow {
    let ui = AppWindow::new().expect("a window");
    pecu_ui::chart::install(&ui);

    let wallet = ui.global::<WalletState>();
    wallet.set_loading(false);
    wallet.set_exists(true);
    wallet.set_locked(false);

    let activity = ui.global::<ActivityState>();
    activity.set_filter(filter.into());
    activity.set_filter_can_fill(pecu_protocol::history_filter_can_fill(filter));

    ui.set_screen("activity".into());
    ui.show().expect("show");
    ui
}

/// Every piece of text on the window, label or not.
///
/// An empty state is plain `Text`, which `accessible_label` alone does not
/// reach — the same reason `tests/balance_captions.rs` reads both.
fn texts(ui: &AppWindow) -> Vec<String> {
    ElementQuery::from_root(ui)
        .match_descendants()
        .find_all()
        .iter()
        .filter_map(|element| {
            element
                .accessible_label()
                .or_else(|| element.accessible_value())
        })
        .map(|text| text.to_string())
        .collect()
}

fn said(ui: &AppWindow, phrase: &str) -> bool {
    texts(ui).iter().any(|text| text.contains(phrase))
}

/// The same, without the repeats, for a failure message somebody has to read.
///
/// The walk visits each element through every branch that reaches it, so the
/// raw list runs to a few hundred entries of which some forty are distinct — and
/// a wall of "Payments", "Payments", "Payments" is the kind of assertion output
/// people stop reading.
fn distinct(ui: &AppWindow) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for text in texts(ui) {
        if !seen.contains(&text) {
            seen.push(text);
        }
    }
    seen
}

/// A tab nothing can reach says what will be in it, and why nothing is.
#[test]
fn a_tab_that_cannot_fill_says_so() {
    let _alone = alone();
    i_slint_backend_testing::init_no_event_loop();

    let ui = activity("login");
    assert!(
        !ui.global::<ActivityState>().get_filter_can_fill(),
        "`login` is a produced kind now — this test is checking the wrong tab, \
         and the one still switched off is the one that needs it",
    );

    assert!(
        said(&ui, "will be listed here"),
        "a tab that cannot have contents is showing an empty list and not \
         saying what would be in it:\n  {:?}",
        distinct(&ui),
    );
    assert!(
        said(&ui, "never reaches a block"),
        "…and not saying why nothing is",
    );
    // The bug as reported: one sentence about the whole history, under a tab
    // showing a slice of it that the wallet has never been able to read.
    assert!(
        !said(&ui, ABOUT_THE_WHOLE_LIST),
        "the Logins tab is answering with the sentence for the whole history",
    );
    // A count is the thing this build cannot honestly offer here. It has never
    // counted a sign-in, so "no sign-ins" would be an answer to a question
    // nobody asked it.
    assert!(
        !said(&ui, "No sign-ins"),
        "a tab that has never counted anything is reporting a count of none",
    );

    ui.hide().expect("hide");
}

/// A tab that can fill is not excused. It is empty because nothing happened.
#[test]
fn a_tab_that_can_fill_is_not_excused() {
    let _alone = alone();
    i_slint_backend_testing::init_no_event_loop();

    let ui = activity("payment");
    assert!(
        ui.global::<ActivityState>().get_filter_can_fill(),
        "payments are what this build produces — if this is false the core has \
         stopped reading history at all",
    );

    assert!(
        said(&ui, "No payments yet"),
        "an empty Payments tab is not saying the plain thing:\n  {:?}",
        distinct(&ui),
    );
    assert!(
        !said(&ui, "will be listed here"),
        "a tab that can fill is excusing itself as one that cannot",
    );

    ui.hide().expect("hide");
}

/// The tab that shows everything keeps the sentence about everything.
///
/// Here because the two above are only a distinction if this one is unchanged:
/// an empty wallet on All is the one case where the whole-history sentence is
/// the true one, and it is what the screen said before this change.
#[test]
fn the_tab_that_shows_everything_still_describes_everything() {
    let _alone = alone();
    i_slint_backend_testing::init_no_event_loop();

    let ui = activity("all");
    assert!(said(&ui, "No transactions yet"), "{:?}", distinct(&ui));
    assert!(said(&ui, ABOUT_THE_WHOLE_LIST), "{:?}", distinct(&ui));

    ui.hide().expect("hide");
}
