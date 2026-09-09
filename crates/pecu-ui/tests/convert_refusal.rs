//! What the review offers after the network has refused the conversion on it.
//!
//! # Why this is a test and not a screenshot
//!
//! `docs/shots/convert-rejected-light.png` is where issue #19 was found, so a
//! reference image plainly can hold this. What it cannot do is hold it against
//! `UPDATE_SNAPSHOTS=1`, which accepts whatever the window happens to draw —
//! including the primary button coming back. The picture is the record; this is
//! the guard.
//!
//! And the thing being asserted is a fact about the *set of actions offered*,
//! not about how they look. A change that kept the confirm on screen and merely
//! restyled it would leave both reference images looking about right and put
//! the refused broadcast back under the cursor.
//!
//! # What "the primary action" is, seen from a test
//!
//! Slint's element tree exposes an element's role, name and enabled state. It
//! does not expose `variant`, so "which button is the accent-coloured one" is
//! not a question this backend can be asked, and a test that claimed to ask it
//! would be reading a colour it cannot see.
//!
//! What it can be asked is which controls exist. That is the stronger property
//! anyway: on this state the confirm is not demoted, it is not offered, and an
//! action that is absent cannot be the primary one.
//!
//! # The one code
//!
//! `convert-refused-by-node` only. There are a dozen other `convert-*` notes
//! and most of them are refusals to *build* a conversion, raised before
//! anything is signed, where re-offering is correct. Which of them should also
//! stop re-offering is the wider half of #19 and is deliberately not decided
//! here — so the positive control below runs the review with no problem on it
//! at all, and nothing in this file says anything about the other codes.

#![allow(clippy::expect_used, clippy::panic)]

use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
use pecu_ui::{AppWindow, ConvertState, WalletState};
use slint::ComponentHandle;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::{Mutex, MutexGuard, PoisonError};

/// The confirm on the convert review: the button that broadcasts.
const CONFIRM: &str = "Convert now";

/// The way off the review without broadcasting.
const BACK: &str = "Back";

/// One window on this process at a time.
///
/// The same reason `tests/balance_captions.rs` has one: the testing backend is
/// process-global, and the threads Cargo runs a file's tests as will deadlock
/// fighting over it. `into_inner` on a poisoned lock so that a test which
/// failed holding it does not turn the rest into lock panics and bury the
/// failure worth reading.
static ONE_WINDOW_AT_A_TIME: Mutex<()> = Mutex::new(());

/// Hold the window to this thread for the rest of the test.
fn alone() -> MutexGuard<'static, ()> {
    ONE_WINDOW_AT_A_TIME
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// The convert screen, shown, in whatever state `fixture` leaves.
fn convert(fixture: fn(&AppWindow)) -> AppWindow {
    let ui = AppWindow::new().expect("a window");
    pecu_ui::chart::install(&ui);
    let wallet = ui.global::<WalletState>();
    wallet.set_loading(false);
    wallet.set_exists(true);
    wallet.set_locked(false);
    fixture(&ui);
    ui.set_screen("convert".into());
    ui.show().expect("show");
    ui
}

/// Every control on screen a person can operate, by the name it announces.
///
/// Buttons only, and read off live elements rather than out of the `.slint`
/// source: a button behind a false `if` is still in the file and is not on the
/// screen, and this is a test about what is on the screen.
fn buttons(ui: &AppWindow) -> Vec<ElementHandle> {
    ElementQuery::from_root(ui)
        .match_descendants()
        .find_all()
        .into_iter()
        .filter(|element| element.accessible_role() == Some(AccessibleRole::Button))
        .collect()
}

/// The one button announcing itself as `label`, if it is on screen.
fn button(ui: &AppWindow, label: &str) -> Option<ElementHandle> {
    buttons(ui)
        .into_iter()
        .find(|element| element.accessible_label().as_deref() == Some(label))
}

/// The review still offers the conversion when nothing has gone wrong with it.
///
/// The positive control, and it is not a formality: without it, deleting the
/// button row outright would pass the test below.
#[test]
fn a_review_nobody_has_refused_still_offers_the_conversion() {
    let _alone = alone();
    i_slint_backend_testing::init_no_event_loop();

    let ui = convert(pecu_ui::fixtures::converting_review);
    let confirm = button(&ui, CONFIRM).expect("the review offers the conversion it is reviewing");
    assert!(
        confirm.accessible_enabled() == Some(true),
        "the review is showing a conversion nobody has objected to and will not send it",
    );
    ui.hide().expect("hide");
}

/// After the network's refusal the broadcast is not on the card at all.
#[test]
fn a_refused_conversion_is_not_offered_for_broadcast_again() {
    let _alone = alone();
    i_slint_backend_testing::init_no_event_loop();

    let ui = convert(pecu_ui::fixtures::converting_rejected);

    // The refusal is what this state is; if it were not on screen the absence
    // below would be a fact about an empty window.
    let said: Vec<String> = ElementQuery::from_root(&ui)
        .match_descendants()
        .find_all()
        .iter()
        .filter_map(ElementHandle::accessible_label)
        .map(|label| label.to_string())
        .collect();
    assert!(
        said.iter().any(|text| text.contains("network refused")),
        "the refusal is not on screen, so this test is checking nothing",
    );

    assert!(
        button(&ui, CONFIRM).is_none(),
        "the network refused this conversion and the card is still offering to \
         broadcast it — the quote underneath it was priced before the refusal",
    );
    ui.hide().expect("hide");
}

/// The way out is still there, still named, and still does something.
///
/// Three separate failures, and the last is the one worth pressing for: this is
/// now the only control on the card, so a Back that announced itself and did
/// nothing would leave somebody driving by screen reader on a dead screen.
#[test]
fn the_way_back_off_a_refused_review_can_be_found_and_pressed() {
    let _alone = alone();
    i_slint_backend_testing::init_no_event_loop();

    let ui = convert(pecu_ui::fixtures::converting_rejected);

    let cancelled = Rc::new(Cell::new(false));
    {
        let cancelled = cancelled.clone();
        ui.global::<ConvertState>()
            .on_cancel(move || cancelled.set(true));
    }

    let back = button(&ui, BACK).expect("a refused review has a way off it");
    assert!(
        back.accessible_enabled() == Some(true),
        "the only remaining control on a refused review is disabled",
    );
    back.invoke_accessible_default_action();
    ui.hide().expect("hide");

    assert!(
        cancelled.get(),
        "pressing Back the way a screen reader does did not ask to leave the \
         review — the only control left on the card is inert",
    );
}
