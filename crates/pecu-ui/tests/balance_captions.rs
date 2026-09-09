//! What the dashboard says about the figures the headline does not contain.
//!
//! # Why this is a test and not a screenshot
//!
//! The wording is a picture's job and the reference images already hold it, so
//! an assertion that merely finds a phrase somewhere on the card is
//! duplication. Two things here are not in any picture.
//!
//! The first is the pairing. A shielded figure without the sentence saying it
//! is outside the total is the whole of issue #7, and today it cannot happen
//! only because the two `Text`s sit in one `VerticalLayout` — a fact about the
//! shape of the row as it stands, not a promise the row makes. So the shielded
//! assertion is adjacency: the caption has to be the next thing on screen
//! after the figure. A rearrangement that left the two of them at opposite
//! ends of the card would pass a test for presence, and fails this one.
//!
//! The second is arithmetic. The ASSETS row for the chain's own currency is
//! spendable, maturing and the shielded pool, which is deliberately not the
//! figure the headline shows — and no PNG can say that the row's number is the
//! headline's plus the private one printed between them. A row that quietly
//! went back to copying the headline would re-record green.
//!
//! The third is the one state no reference image renders: money leaving. No
//! fixture sets `pending`, and adding one would be an eleventh picture of a
//! state nobody asked to see — so the column carrying the opposite sentence to
//! its neighbours is checked here or nowhere.
//!
//! What the rest of the file buys is insurance against re-recording.
//! `UPDATE_SNAPSHOTS=1` accepts whatever the window happens to draw, including
//! a caption that has drifted onto a figure it is false about. These
//! assertions do not.

#![allow(clippy::expect_used, clippy::panic)]

mod support;

use i_slint_backend_testing::ElementQuery;
use pecu_ui::{AppWindow, WalletState};
use slint::ComponentHandle;
use support::window_to_read;

/// Said of the shielded pool: a separate pool, never in the headline.
const OUTSIDE: &str = "not counted above";

/// Said of arriving money: the same pool, one confirmation away.
const NOT_YET: &str = "not counted yet";

/// Said of leaving money: the headline has already taken it off.
const ALREADY_GONE: &str = "already deducted";

/// What the ASSETS row for the chain's own currency counts, once a scan has
/// found a shielded balance to fold in.
const ROW_WITH_SHIELDED: &str = "spendable + maturing + shielded";

/// …and what it counts when no scan has produced one. Not a claim that the
/// pool is empty — a claim about this figure.
const ROW_WITHOUT_SHIELDED: &str = "spendable + maturing";

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

/// A funded wallet on the dashboard, shown, in whatever state `fixture` leaves.
fn dashboard(fixture: fn(&AppWindow)) -> AppWindow {
    let ui = unlocked();
    fixture(&ui);
    ui.set_screen("dashboard".into());
    ui.show().expect("show");
    ui
}

/// Every piece of text on the window, label or not, in the order it is drawn.
///
/// These captions are plain `Text`, which `accessible_label` alone does not
/// reach — the same reason `tests/tokens.rs` and `tests/translation.rs` read
/// both.
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
/// `find_all` walks the element tree in the order the window is written, so
/// two `Text`s that are siblings in one `VerticalLayout` come out of it side
/// by side, and a caption that has moved elsewhere on the card does not.
fn said_beside(ui: &AppWindow, figure: &str, caption: &str) -> bool {
    texts(ui)
        .windows(2)
        .any(|pair| pair[0].contains(figure) && pair[1].contains(caption))
}

#[test]
fn the_shielded_figure_is_never_shown_without_saying_it_is_outside_the_total() {
    let _turn = window_to_read();

    let ui = dashboard(pecu_ui::fixtures::funded_with_shielded);
    assert!(
        said(&ui, "2.5000 0000"),
        "the shielded fixture is not showing its figure, so this test is checking nothing",
    );
    assert!(
        said_beside(&ui, "2.5000 0000", OUTSIDE),
        "the shielded balance and the sentence putting it outside the total have come apart",
    );
    ui.hide().expect("hide");
}

/// The ASSETS row for the chain's own currency counts the shielded pool, and
/// the two figures on this screen therefore differ.
///
/// # Why this is a test and not a screenshot
///
/// The pictures hold the wording and the layout, and re-recording them accepts
/// whatever the window drew. What they cannot hold is the *arithmetic*: nothing
/// in a PNG says that 12 484.92 is 12 482.42 plus the 2.50 printed above it, so
/// a row that quietly went back to the headline's figure — or a caption that
/// stayed while the figure moved — would re-record green.
///
/// So this asserts the relationship between three numbers on one screen and the
/// adjacency of the sentence that explains it, which is the pairing
/// `the_shielded_figure_is_never_shown_without_saying_it_is_outside_the_total`
/// makes for the hero card, one card lower.
#[test]
fn the_native_asset_row_counts_the_shielded_pool_and_says_that_is_what_it_counts() {
    let _turn = window_to_read();

    let ui = dashboard(pecu_ui::fixtures::funded_with_shielded);

    // The headline, unchanged by this: spendable plus maturing, with the
    // shielded pool beside it rather than inside it.
    assert!(
        said(&ui, "12 482.4200 0000"),
        "the headline total is not on screen, so this test is checking nothing",
    );
    assert!(
        said(&ui, "2.5000 0000"),
        "the shielded figure is not on screen, so there is nothing for the row to have added",
    );
    // And the row, which is those two plus the pool. Written as the sum it is
    // rather than as a literal nobody can check: 12 482.42 + 2.50.
    assert!(
        said(&ui, "12 484.9200 0000"),
        "the asset row is not showing spendable + maturing + shielded — it is either \
         still a copy of the headline, or it is some fourth number",
    );

    // The caption is the last thing before the figure it describes. `ListRow`
    // draws title, subtitle, then whatever was handed to it as a child, so a
    // caption that had drifted onto another row — or onto the token row, whose
    // subtitle is an i-address — does not land here.
    assert!(
        said_beside(&ui, ROW_WITH_SHIELDED, "12 484.9200 0000"),
        "the asset row shows a figure the headline does not, with nothing beside it \
         saying what it counts — two different totals on one screen and no explanation",
    );
    ui.hide().expect("hide");
}

/// …and a row with no scanned pool behind it does not claim to have counted one.
///
/// The other half, and the one that keeps the caption honest. An unscanned
/// shielded account is not a balance of zero — `ShieldedFunds` exists to hold
/// that distinction — so the row neither adds one nor says it did. The flag and
/// the amount are set one line apart in `Reading::portfolio` for exactly this
/// reason, and this is what would fail if they ever stopped being.
#[test]
fn a_row_with_no_scanned_pool_behind_it_does_not_say_it_counted_one() {
    let _turn = window_to_read();

    let ui = dashboard(pecu_ui::fixtures::funded);

    // Nothing has been scanned here, so the row is the headline's own two
    // figures and reads as the same number — which is correct, and is now a
    // statement the row makes rather than a coincidence.
    assert!(
        said_beside(&ui, ROW_WITHOUT_SHIELDED, "12 482.4200 0000"),
        "the asset row is not saying what it counts",
    );
    assert!(
        !said(&ui, ROW_WITH_SHIELDED),
        "the row is claiming to have counted a shielded pool nobody has looked in",
    );
    ui.hide().expect("hide");
}

#[test]
fn a_pool_nobody_has_looked_in_is_not_labelled_as_missing_from_the_total() {
    let _turn = window_to_read();

    // `funded` leaves `shielded-any` false, which is the state of an unscanned
    // account and of a scanned empty one alike — the two the row is written to
    // stay out of. Both reference images of this fixture show the same thing;
    // what they cannot do is say it, and a re-record cannot ask itself whether
    // a caption that appeared here was meant to.
    let ui = dashboard(pecu_ui::fixtures::funded);
    assert!(
        said(&ui, "spendable"),
        "the breakdown is not on screen, so the absence below is a fact about an empty window",
    );
    assert!(
        !said(&ui, OUTSIDE),
        "the wallet is excusing a shielded balance it does not have and has not looked for",
    );
    ui.hide().expect("hide");
}

#[test]
fn money_on_its_way_is_marked_as_not_being_in_the_total_yet() {
    let _turn = window_to_read();

    let ui = dashboard(pecu_ui::fixtures::funded);
    assert!(
        said(&ui, "5.0000 0000"),
        "the funded fixture is not showing an arriving figure, so this test is checking nothing",
    );
    assert!(
        said_beside(&ui, "5.0000 0000", NOT_YET),
        "an arriving payment is on screen with nothing beside it to say the total omits it",
    );
    // Names the distinction at the point it is made, which is cheap: arriving
    // money joins the total on confirmation, shielded money never does.
    assert!(
        !said(&ui, OUTSIDE),
        "arriving money is being described as permanently outside the total",
    );
    ui.hide().expect("hide");
}

#[test]
fn money_already_taken_off_the_total_does_not_claim_to_be_missing_from_it() {
    let _turn = window_to_read();

    // Set from the test rather than in the fixture: no reference image renders
    // a wallet with something leaving, and adding one would be a picture of a
    // state nobody asked to see. The caption still needs a guard, because it is
    // the one of the three whose sentence is the opposite of its neighbours'.
    let ui = unlocked();
    pecu_ui::fixtures::funded(&ui);
    ui.global::<WalletState>().set_pending("3.0000 0000".into());
    ui.set_screen("dashboard".into());
    ui.show().expect("show");

    assert!(
        said_beside(&ui, "3.0000 0000", ALREADY_GONE),
        "money leaving is on screen with nothing beside it to say the total has lost it",
    );
    // Nothing on this screen is outside the total permanently: the shielded
    // pool is empty here, and both the other figures are the same coin at a
    // different moment. A stray "not counted above" would mean one of the two
    // captions had been given the shielded sentence.
    assert!(
        !said(&ui, OUTSIDE),
        "a figure that is not the shielded pool is claiming to sit outside the total",
    );

    // Four columns is the widest this row is ever drawn, the captions grew it
    // by some 190px, and no picture has it — so it was measured here once, in
    // the 1240-wide window the reference images use. The leaving caption ends
    // at x=872 and the Send button begins at x=896; raising spendable to
    // "1 112 382.4200 0000" slides the two right together, to 897 and 921 with
    // the button ending at 1021, rather than squeezing either. There is no
    // assertion on that above, because a layout shrinks its children instead
    // of overlapping them: a right-edge comparison cannot fail and would only
    // look like a guard.
    ui.hide().expect("hide");
}
