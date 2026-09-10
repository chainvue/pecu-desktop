//! What the assets card says when nobody could count the tokens.
//!
//! # Why this is a test and not a screenshot
//!
//! Because the whole of issue #34 is a difference between two screens that
//! photograph almost identically. A wallet holding only the chain's own coin
//! and a wallet whose read could not reach an address draw the same single row,
//! and the only thing separating them is one line of micro text under it. Two
//! reference images hold that line — and what a reference image cannot do is
//! say which of the two states it was taken in. `UPDATE_SNAPSHOTS=1` accepts
//! whatever the window drew: a sentence that had started appearing under every
//! token-free wallet, telling somebody who holds no tokens that their tokens
//! could not be counted, would re-record green and look right.
//!
//! So both directions are asserted here. The sentence is present in the state
//! the core flagged, and absent in the state it did not — which is the pair,
//! and the pair is the feature. Neither half means anything alone: a caption
//! that is always there has not distinguished anything, and one that is never
//! there is the silence this replaces.
//!
//! Adjacency, in the style of `tests/balance_captions.rs`: the sentence has to
//! be the next thing on screen after the row it is about. A caption drawn
//! somewhere else on the card would pass a test for presence, and is a sentence
//! about a list sitting beside a different list.

#![allow(clippy::expect_used, clippy::panic)]

use i_slint_backend_testing::ElementQuery;
use pecu_ui::{AppWindow, AssetRow, WalletState};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::rc::Rc;
use std::sync::{Mutex, MutexGuard, PoisonError};

/// The half of the sentence that is the claim: the list is not a denial.
///
/// Matched on a fragment rather than the whole line so that rewording the
/// reason does not break this, while losing the distinction does.
const NOT_A_DENIAL: &str = "not saying there are none";

/// The chain's own row in the `funded` fixture, which is also the only row a
/// wallet that could not count its tokens has.
const NATIVE_AMOUNT: &str = "12 482.4200 0000";

/// What that row is worth, which is the *last* text the row draws.
///
/// The adjacency assertion anchors here rather than on the amount: the row
/// stacks its value under its amount, so the amount is no longer the thing
/// immediately before the sentence under the list. Presence of the row is
/// still asserted on the amount, which is the figure the row is about.
const NATIVE_VALUE: &str = "6 705.88";

/// One window on this process at a time.
///
/// The same reason `tests/balance_captions.rs` and `tests/tokens.rs` have one:
/// the testing backend is process-global, and the threads Cargo runs a file's
/// tests as deadlock fighting over it. `into_inner` on a poisoned lock so that
/// a test which failed holding it does not bury its own failure under lock
/// panics in the tests after it.
static ONE_WINDOW_AT_A_TIME: Mutex<()> = Mutex::new(());

/// Hold the window to this thread for the rest of the test.
fn alone() -> MutexGuard<'static, ()> {
    ONE_WINDOW_AT_A_TIME
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// The shell, on screen rather than the unlock form.
fn unlocked() -> AppWindow {
    let ui = AppWindow::new().expect("a window");
    pecu_ui::chart::install(&ui);
    let wallet = ui.global::<WalletState>();
    wallet.set_loading(false);
    wallet.set_exists(true);
    wallet.set_locked(false);
    ui
}

/// A wallet on the dashboard, shown, in whatever state `fixture` leaves.
fn dashboard(fixture: fn(&AppWindow)) -> AppWindow {
    let ui = unlocked();
    fixture(&ui);
    ui.set_screen("dashboard".into());
    ui.show().expect("show");
    ui
}

/// Every piece of text on the window, in the order it is drawn.
///
/// Both halves, because these captions are plain `Text` and
/// `accessible_label` alone does not reach one — the same reason
/// `tests/balance_captions.rs` reads both.
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

/// Whether anything on screen contains this phrase.
fn said(ui: &AppWindow, phrase: &str) -> bool {
    texts(ui).iter().any(|text| text.contains(phrase))
}

/// Whether `caption` is the very next thing on screen after `figure`.
///
/// `find_all` walks the element tree in the order the window is written, so a
/// caption that is a sibling of the rows it describes comes out of it directly
/// after the last of them, and one that has moved elsewhere on the card does
/// not.
fn said_beside(ui: &AppWindow, figure: &str, caption: &str) -> bool {
    texts(ui)
        .windows(2)
        .any(|pair| pair[0].contains(figure) && pair[1].contains(caption))
}

/// The same wallet with the token read having succeeded and found nothing.
///
/// Built from `funded` by dropping the token row rather than by writing a row
/// out here: this has to be the *same* list the fixture above shows, or the two
/// tests are comparing two different screens and the difference they attribute
/// to the flag is something else. Taken off the model for the same reason the
/// fixture does it — a hand-written `AssetRow` here would be a third copy of a
/// row to keep in step.
fn holding_no_tokens() -> AppWindow {
    let ui = unlocked();
    pecu_ui::fixtures::funded(&ui);

    let wallet = ui.global::<WalletState>();
    let native: Vec<AssetRow> = wallet.get_assets().iter().take(1).collect();
    wallet.set_assets(ModelRc::from(Rc::new(VecModel::from(native))));
    wallet.set_holds_tokens(false);

    ui.set_screen("dashboard".into());
    ui.show().expect("show");
    ui
}

#[test]
fn a_list_that_could_not_count_the_tokens_says_so_under_the_row_it_is_about() {
    let _alone = alone();
    i_slint_backend_testing::init_no_event_loop();

    let ui = dashboard(pecu_ui::fixtures::tokens_uncounted);

    assert!(
        said(&ui, NATIVE_AMOUNT),
        "the assets card is not showing its one row, so this test is checking nothing",
    );
    assert!(
        said_beside(&ui, NATIVE_VALUE, NOT_A_DENIAL),
        "a wallet that could not count its tokens is showing a list with no token in it \
         and nothing after the row to say the list is not a statement that there are none",
    );
    ui.hide().expect("hide");
}

#[test]
fn a_wallet_that_holds_no_tokens_is_not_told_its_tokens_could_not_be_counted() {
    let _alone = alone();
    i_slint_backend_testing::init_no_event_loop();

    let ui = holding_no_tokens();

    assert!(
        said(&ui, NATIVE_AMOUNT),
        "the assets card is not showing its one row, so the absence below is a fact \
         about an empty window",
    );
    assert!(
        !said(&ui, NOT_A_DENIAL),
        "a wallet whose read finished and found no tokens is being told its tokens \
         could not be counted — the sentence is drawn for both states, which is the \
         same silence it was meant to break",
    );
    ui.hide().expect("hide");
}
