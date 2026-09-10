//! What the assets card says a holding is worth.
//!
//! # Why this is a test and not a screenshot
//!
//! The reference images hold the wording, the rounding and the column, and
//! `UPDATE_SNAPSHOTS=1` accepts whatever the window drew. Three things in this
//! column cannot be held that way.
//!
//! **The pairing of a figure with its own amount.** A value is only meaningful
//! beside the holding it values, and nothing in a PNG says that `6 705.88` is
//! what the row above it is worth rather than what some other row is. So the
//! assertion is adjacency, in the style of `tests/balance_captions.rs`: the
//! value has to be the next thing on screen after the amount it belongs to. A
//! rearrangement that drew the values in one block and the amounts in another
//! would photograph plausibly and fails here.
//!
//! **The unit, which is drawn once.** This column is the narrowest of the
//! three, so the figures carry no currency and the heading carries it for all
//! of them. That is a deliberate trade and it has one failure mode: drop the
//! heading and every value on the card becomes a number with no denomination,
//! which still renders, still reviews, and means nothing. The heading is
//! asserted here because it is load-bearing somewhere else on the card.
//!
//! **The em dash.** An unpriced holding is the state no reference image can
//! carry: every currency in `fixtures::funded` is one the scripted chain's book
//! prices, and a picture showing `—` beside a row the MARKETS card two columns
//! over is quoting a price for would be incoherent. So the state that matters
//! most — the wallet not knowing — is checked here or nowhere. A zero in its
//! place would be the wallet saying somebody's holding is worthless.

#![allow(clippy::expect_used, clippy::panic)]

use i_slint_backend_testing::ElementQuery;
use pecu_ui::{AppWindow, AssetRow, MarketState, WalletState};
use slint::{ComponentHandle, Model};
use std::sync::{Mutex, MutexGuard, PoisonError};

/// The chain's own row in `fixtures::funded`, and what it is worth.
///
/// 12 482.42 at the 0.5372 DAI.vETH the MARKETS card is showing on the same
/// screen. Written as two constants rather than one assertion about a product,
/// because the multiplication is `Book::value_of`'s and `pecu-core`'s
/// `the_dashboard_values_what_it_holds_from_the_book_the_markets_table_shows`
/// is where it is checked against the table. What is checked here is that the
/// two of them arrive together.
const NATIVE_AMOUNT: &str = "12 482.4200 0000";
const NATIVE_VALUE: &str = "6 705.88";

/// The token row, which is the one the issue is actually about: 48.50 at 7.09.
const TOKEN_AMOUNT: &str = "48.5000 0000";
const TOKEN_VALUE: &str = "343.88";

/// The same holding with the shielded pool folded in, and what *that* is worth.
const SHIELDED_AMOUNT: &str = "12 484.9200 0000";
const SHIELDED_VALUE: &str = "6 707.22";

/// The em dash this interface uses for a figure it does not have.
const UNKNOWN: &str = "—";

/// The currency every value on the card is in, named where the card names it.
const QUOTE: &str = "DAI.vETH";

/// One window on this process at a time.
///
/// The same reason `tests/balance_captions.rs` and `tests/tokens.rs` have one:
/// the testing backend is process-global, and the threads Cargo runs a file's
/// tests as deadlock fighting over it. `into_inner` on a poisoned lock so that a
/// test which failed holding it does not bury its own failure under lock panics
/// in the tests after it.
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

/// A funded wallet on the dashboard, shown, in whatever state `fixture` leaves.
fn dashboard(fixture: fn(&AppWindow)) -> AppWindow {
    let ui = unlocked();
    fixture(&ui);
    ui.set_screen("dashboard".into());
    ui.show().expect("show");
    ui
}

/// Every piece of text on the window, in the order it is drawn.
///
/// Both labels and values, for the reason `tests/balance_captions.rs` gives:
/// the heading is a plain `Text`, which `accessible_label` alone does not reach.
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

/// Whether `second` is the very next thing on screen after `first`.
///
/// `find_all` walks the element tree in the order the window is written, so two
/// figures stacked in one `VerticalLayout` come out of it side by side, and one
/// that has moved elsewhere on the card does not.
fn said_beside(ui: &AppWindow, first: &str, second: &str) -> bool {
    texts(ui)
        .windows(2)
        .any(|pair| pair[0].contains(first) && pair[1].contains(second))
}

/// Every holding carries what it is worth, on the row it belongs to.
#[test]
fn a_holding_is_shown_beside_what_it_is_worth() {
    let _alone = alone();
    i_slint_backend_testing::init_no_event_loop();

    let ui = dashboard(pecu_ui::fixtures::funded);

    assert!(
        said(&ui, NATIVE_AMOUNT) && said(&ui, TOKEN_AMOUNT),
        "the assets card is not showing its two rows, so this test is checking nothing",
    );
    assert!(
        said_beside(&ui, NATIVE_AMOUNT, NATIVE_VALUE),
        "the chain's own holding is not followed by what it is worth",
    );
    // The token row is the one issue #14 is about: "what is my Bridge.vETH
    // actually worth" was a trip to Markets and a multiplication.
    assert!(
        said_beside(&ui, TOKEN_AMOUNT, TOKEN_VALUE),
        "the token holding is not followed by what it is worth — which is the \
         question this column was added to answer",
    );
    ui.hide().expect("hide");
}

/// The figures have no unit on them, so the card has to name it.
///
/// Half of a deliberate trade: ASSETS is the narrowest of the three dashboard
/// columns, `DAI.vETH` on every row is nine characters three times over in a
/// column that is already eliding an i-address, and the heading has the width
/// for it. The half that is easy to lose is this one, and a column of
/// denominated figures with the denomination gone is a column of numbers that
/// mean nothing.
#[test]
fn the_card_names_the_currency_its_values_are_in() {
    let _alone = alone();
    i_slint_backend_testing::init_no_event_loop();

    let ui = dashboard(pecu_ui::fixtures::funded);
    assert!(
        said(&ui, &format!("ASSETS · WORTH IN {QUOTE}")),
        "the values on this card are in {QUOTE} and nothing on it says so",
    );

    // And it is silent before there is a book, rather than promising a currency
    // nothing is priced in. Clearing the quote is what a wallet that has not
    // read a converter yet looks like, and every value is `—` in that state.
    ui.global::<MarketState>().set_quote("".into());
    assert!(
        !said(&ui, "WORTH IN"),
        "the card is naming a currency it has no prices in",
    );
    ui.hide().expect("hide");
}

/// The value follows the amount it is a price of, not the headline above it.
///
/// The shielded picture is where this can be seen: #38 put the private pool
/// into the ASSETS row and deliberately not into the hero figure, so the row
/// shows 12 484.92 where the headline shows 12 482.42. The value has to be the
/// row's figure priced, and 2.50 of VRSCTEST is 1.34 of DAI.vETH — the
/// difference between the two candidate answers is larger than the rounding, so
/// this distinguishes them.
#[test]
fn the_value_prices_the_figure_on_the_row_and_not_the_one_above_it() {
    let _alone = alone();
    i_slint_backend_testing::init_no_event_loop();

    let ui = dashboard(pecu_ui::fixtures::funded_with_shielded);
    assert!(
        said_beside(&ui, SHIELDED_AMOUNT, SHIELDED_VALUE),
        "the row counting the shielded pool is not valued at what it counts",
    );
    assert!(
        !said(&ui, NATIVE_VALUE),
        "the row is priced from the headline's figure rather than its own",
    );
    ui.hide().expect("hide");
}

/// A holding the wallet cannot price says so, and does not say zero.
///
/// # Why the em dash is the whole point
///
/// A currency no started basket holds has no price on this chain — there is
/// nothing to derive one from — and it is still a currency somebody holds. Zero
/// is the wrong answer twice over: it is a figure where there is none, and the
/// figure it invents is the one that says the holding is worthless. `—` is what
/// every other unknown in this interface says.
///
/// # How this composes with the sentence under the list
///
/// Issue #34 adds a line below the rows for a read that could not count the
/// tokens. The two are about different absences and they read as one statement:
/// a row with an amount and `—` says "you hold this, and I cannot price it",
/// while that sentence says "there may be rows missing entirely". Neither
/// borrows the other's meaning, and a row that said `0.00` here would contradict
/// both.
#[test]
fn a_holding_nobody_can_price_shows_no_figure_rather_than_a_zero() {
    let _alone = alone();
    i_slint_backend_testing::init_no_event_loop();

    let ui = dashboard(pecu_ui::fixtures::funded);

    // The same two rows, with the token's value taken away — which is what the
    // core sends for a currency its book cannot reach. Edited off the model
    // rather than written out here, so this is the row every other test and
    // every reference image shows, in one respect different.
    let wallet = ui.global::<WalletState>();
    let rows: Vec<AssetRow> = wallet
        .get_assets()
        .iter()
        .map(|row| AssetRow {
            value: if row.native {
                row.value.clone()
            } else {
                UNKNOWN.into()
            },
            ..row
        })
        .collect();
    wallet.set_assets(slint::ModelRc::from(std::rc::Rc::new(
        slint::VecModel::from(rows),
    )));

    assert!(
        said_beside(&ui, TOKEN_AMOUNT, UNKNOWN),
        "an unpriced holding is not saying that it is unpriced",
    );
    assert!(
        !said_beside(&ui, TOKEN_AMOUNT, "0.00"),
        "an unpriced holding is being shown as worth nothing, which is a claim \
         about somebody's money that no price supports",
    );
    // The priced row beside it is untouched: the column is per holding, not per
    // card, and one unknown does not blank the rest.
    assert!(
        said_beside(&ui, NATIVE_AMOUNT, NATIVE_VALUE),
        "one unpriced row has taken the value off a row that had one",
    );
    ui.hide().expect("hide");
}

/// The dashboard's percentages say what window they are over.
///
/// The markets screen heads the same column `30d`, and the comment there
/// explains why the label is not decoration: the series is sampled once a day,
/// so the same figures under a `24h` heading would look precise and not be. This
/// panel showed them with no heading at all, which leaves a bare percentage —
/// and a bare percentage on a market row reads as a day's move, that being what
/// it means on every other market screen anybody has seen.
#[test]
fn the_dashboard_says_what_window_its_percentages_are_over() {
    let _alone = alone();
    i_slint_backend_testing::init_no_event_loop();

    let ui = dashboard(pecu_ui::fixtures::funded);
    assert!(
        said(&ui, "30d"),
        "the dashboard's markets panel is showing percentages with no window on them",
    );
    ui.hide().expect("hide");
}
