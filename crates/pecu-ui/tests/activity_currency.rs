//! Which money the Activity screen is counting, and that it says so once.
//!
//! # Why this is a test and not a screenshot
//!
//! A reference image holds the wording and the position, and holds them better
//! than an assertion can. Two things here are not in any picture.
//!
//! The first is where the word comes from. Every reference image of this
//! screen is rendered from a fixture on one chain, so a picture cannot tell a
//! ticker read from `WalletState.ticker` apart from the same letters typed
//! into the `.slint` — which is issue #17, one screen along, and the reason
//! this one is stacked on its fix. So the chain is set from here, to one no
//! activity fixture mentions, and the screen has to come back saying it: a
//! literal scores zero and is caught, and a ticker that drifted back onto the
//! rows scores six and is caught too.
//!
//! The second is the empty case. `WalletState.ticker` is empty until a node
//! names the chain, and no fixture photographs an Activity screen in that
//! state. "Amounts are in ." is a sentence with a hole in it that would render
//! perfectly, review perfectly, and appear in no image anybody looks at.
//!
//! What all three buy, as `balance_captions.rs` puts it, is insurance against
//! re-recording: `UPDATE_SNAPSHOTS=1` accepts whatever the window happens to
//! draw, and these assertions do not.

#![allow(clippy::expect_used, clippy::panic)]

use i_slint_backend_testing::ElementQuery;
use pecu_ui::{AppWindow, WalletState};
use slint::ComponentHandle;
use std::collections::HashSet;
use std::sync::{Mutex, MutexGuard, PoisonError};

/// The fixed half of the sentence that names the currency.
///
/// Matched on rather than the whole line, so that rewording the tail — a
/// designer's and a translator's business — does not fail a test about where
/// the ticker comes from and how often it is drawn.
const NAMES_THE_CURRENCY: &str = "Amounts are in";

/// Two chains this wallet ships and no activity fixture mentions.
///
/// Real names rather than invented ones, because the reason this screen has to
/// say anything at all is that one interface renders five of them. Neither
/// string occurs in a fixture row's amount, note or day heading, so a count of
/// one of them on this screen is a count of the line under test.
const ONE_CHAIN: &str = "vARRR";
const ANOTHER_CHAIN: &str = "CHIPS";

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

/// The Activity screen, with a full history on it, on the chain named.
///
/// `history` is the fixture the reference images use: four kinds of row, three
/// day headings and the summary strip, which is the most crowded this screen
/// gets and so the most places a stray ticker could hide. The chain is
/// overridden after it, because every fixture in this crate is on VRSCTEST and
/// a test that accepted the fixture's own answer would pass against a
/// hard-coded one.
fn activity_on(chain: &str) -> AppWindow {
    let ui = AppWindow::new().expect("a window");

    // Explicitly, and for the reason `tests/translation.rs` sets out: Slint
    // picks its bundled language from the **system locale**, so on a German
    // machine this window comes up in German. Nothing asserted below is a
    // navigation label, but the sentence under test is one `@tr` away from
    // being catalogued, and a test that passes or fails by locale is worse
    // than one that is wrong everywhere.
    slint::select_bundled_translation("en").expect("the default language");

    pecu_ui::chart::install(&ui);
    pecu_ui::fixtures::history(&ui);
    ui.global::<WalletState>().set_ticker(chain.into());
    ui.show().expect("show");
    ui
}

/// Every piece of text on the window, once for each place it is drawn.
///
/// Two things are going on here.
///
/// The line under test is a plain `Text`, which `accessible_label` alone does
/// not reach — the same reason `tests/balance_captions.rs` and
/// `tests/translation.rs` read the value as well as the label.
///
/// And `find_all` hands the same element back more than once. On this screen
/// it is six times each: 2727 handles for a window with a few hundred elements
/// in it, and the six copies of this caption report one position and one size
/// between them. That is a property of the query rather than of the window, and
/// no existing test noticed because none of them counts — `translation.rs`
/// asserts `>= 2` where the honest answer is two. A count is the whole point
/// here, so the handles are folded by what they say and where they are drawn.
/// Two mentions are two mentions when they are in two places on the glass.
fn drawn(ui: &AppWindow) -> Vec<String> {
    let mut seen = HashSet::new();
    ElementQuery::from_root(ui)
        .match_descendants()
        .find_all()
        .iter()
        .filter_map(|element| {
            let text = element
                .accessible_label()
                .or_else(|| element.accessible_value())?;
            let at = element.absolute_position();
            let size = element.size();
            let once = seen.insert((
                text.to_string(),
                at.x.to_bits(),
                at.y.to_bits(),
                size.width.to_bits(),
                size.height.to_bits(),
            ));
            once.then(|| text.to_string())
        })
        .collect()
}

/// How many places on screen contain this phrase.
fn times_said(ui: &AppWindow, phrase: &str) -> usize {
    drawn(ui)
        .iter()
        .filter(|text| text.contains(phrase))
        .count()
}

#[test]
fn the_activity_screen_names_its_currency_exactly_once() {
    let _alone = alone();
    i_slint_backend_testing::init_no_event_loop();

    let ui = activity_on(ONE_CHAIN);

    // "Once" is a claim about something only when there are several places it
    // could have been said. This fixture draws six rows and a four-figure
    // summary strip: before this line existed the count below was zero, and a
    // ticker back on the rows would make it six.
    assert!(
        times_said(&ui, "0000") >= 4,
        "the activity list is not on screen, so a count of tickers on it counts nothing: {:?}",
        drawn(&ui)
    );
    assert_eq!(
        times_said(&ui, ONE_CHAIN),
        1,
        "the Activity screen should name its currency exactly once; got {:?}",
        drawn(&ui)
    );
    assert!(
        drawn(&ui)
            .iter()
            .any(|text| text.contains(NAMES_THE_CURRENCY) && text.contains(ONE_CHAIN)),
        "the one mention is not the sentence that names the currency: {:?}",
        drawn(&ui)
    );

    ui.hide().expect("hide");
}

#[test]
fn the_currency_named_is_the_chain_the_wallet_is_on() {
    let _alone = alone();
    i_slint_backend_testing::init_no_event_loop();

    // The point of the pair. A literal in the `.slint` renders identically to
    // a property on any one chain, and this wallet runs on five: the screen
    // has to follow the ticker to the other one and leave nothing of the first
    // behind.
    for (shown, absent) in [(ONE_CHAIN, ANOTHER_CHAIN), (ANOTHER_CHAIN, ONE_CHAIN)] {
        let ui = activity_on(shown);
        assert_eq!(
            times_said(&ui, shown),
            1,
            "on {shown} the screen should name {shown} once; got {:?}",
            drawn(&ui)
        );
        assert_eq!(
            times_said(&ui, absent),
            0,
            "on {shown} the screen is still naming {absent}; got {:?}",
            drawn(&ui)
        );
        ui.hide().expect("hide");
    }
}

#[test]
fn a_chain_no_node_has_named_yet_leaves_no_gap_in_the_sentence() {
    let _alone = alone();
    i_slint_backend_testing::init_no_event_loop();

    // The state a running wallet is in before its first portfolio arrives, and
    // one no reference image renders. `bridge::apply_wallet` clears `loading`
    // on unlock while `apply_portfolio` is what fills the ticker, so this is
    // reachable with the rest of the screen fully drawn — which is why the
    // line is guarded on the ticker itself rather than on `loading`.
    let ui = activity_on("");

    assert!(
        times_said(&ui, "0000") >= 4,
        "the activity list is not on screen, so the absence below is a fact about an empty window: {:?}",
        drawn(&ui)
    );
    assert_eq!(
        times_said(&ui, NAMES_THE_CURRENCY),
        0,
        "an unnamed chain should get no sentence at all rather than one with a hole in it; got {:?}",
        drawn(&ui)
    );

    ui.hide().expect("hide");
}
