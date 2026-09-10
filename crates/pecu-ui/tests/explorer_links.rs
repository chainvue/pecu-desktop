//! One convention for a transaction id, on every screen that shows one.
//!
//! # Why this is a test and not a screenshot
//!
//! Because the bug it pins is an *agreement between two files*, and a reference
//! image can only ever photograph one screen at a time. Both pictures were
//! correct before issue #18: the send screen really did offer "Open in the
//! block explorer", the transaction sheet really did offer "Copy link", and
//! nothing about either image said the other one existed. What was wrong was
//! that a person met both and could not tell what the wallet believed.
//!
//! So this asserts the affordances by name, on every screen that ends with a
//! transaction id, from one list — which is a shape that cannot pass while the
//! two screens disagree. It fails against the commit before this one on every
//! screen at once: on the sheet because nothing on it opened anything, and on
//! send and convert because nothing on them copied a link.
//!
//! # And why it presses them
//!
//! A label is a claim. Issue #18's real risk is not a missing button, it is a
//! button that opens something the wallet did not build — the concern
//! `activity.slint` recorded when it declined to open a browser at all. The
//! last test here presses "open" the way a screen reader does and reads back
//! what the interface asked to open, which is the assertion that the URL
//! reaching `pecu-app`'s `is_openable` is the explorer link and not something a
//! node put in a reply.

#![allow(clippy::expect_used, clippy::panic)]

use i_slint_backend_testing::{ElementHandle, ElementQuery};
use pecu_ui::{Actions, AppWindow, WalletState};
use slint::ComponentHandle;

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

/// Every screen this wallet ends a transaction on, and the URL it should offer.
///
/// The convert screens are here for the same reason the send ones are: they
/// share a component now, and a list that only held the two the issue named
/// would go green on a change that reached one of them and not the other.
///
/// The transaction sheet is the screen the issue is about. It is seeded over
/// Activity, which is where it opens from.
type Case = (&'static str, &'static str, fn(&AppWindow));

const SCREENS: &[Case] = &[
    (
        "send → sent",
        "https://markets.chainvue.io/testnet/tx/\
         68320bb5eb723ca3ab3f92d26133b4309d03c59e9ce3e93dba85d68379e98883/",
        pecu_ui::fixtures::sent,
    ),
    (
        "send → uncertain",
        "https://markets.chainvue.io/testnet/tx/\
         68320bb5eb723ca3ab3f92d26133b4309d03c59e9ce3e93dba85d68379e98883/",
        pecu_ui::fixtures::send_uncertain,
    ),
    (
        "convert → sent",
        "https://markets.chainvue.io/testnet/tx/\
         6a3f9c1e2b7d4f8a0c5e1937b6d2f4a8c9e0173b5d8f2a4c6e91b3d7f5a8c0e2/",
        pecu_ui::fixtures::converting_sent,
    ),
    (
        "the transaction sheet",
        "https://testex.verus.io/tx/\
         685ffac53fc525a4cefa5ed334139aebace508cbe293a41e6edba096f22517a5",
        pecu_ui::fixtures::tx_detail,
    ),
];

/// The three controls a transaction id affords, by the name each announces.
///
/// Written out rather than derived, because the point of the issue is that the
/// wording is the same everywhere: a test that read the labels off one screen
/// and compared them to another would pass on two screens that had drifted
/// together into a third wrong convention.
const AFFORDANCES: [&str; 3] = [
    "Copy the transaction id",
    "Copy the link to this transaction",
    "Open this transaction in the block explorer",
];

/// The elements on this window announcing themselves by `label`.
fn named(ui: &AppWindow, label: &str) -> Vec<ElementHandle> {
    ElementQuery::from_root(ui)
        .match_descendants()
        .find_all()
        .into_iter()
        .filter(|element| element.accessible_label().as_deref() == Some(label))
        .collect()
}

/// A window with one of the screens above on it, laid out and shown.
fn showing(seed: fn(&AppWindow)) -> AppWindow {
    let ui = unlocked();
    seed(&ui);
    ui.show().expect("show");
    ui
}

/// Copying and opening are both offered, on every screen, under one wording.
#[test]
fn every_transaction_id_can_be_copied_and_opened() {
    i_slint_backend_testing::init_no_event_loop();

    for (screen, _, seed) in SCREENS {
        let ui = showing(*seed);

        for affordance in AFFORDANCES {
            assert!(
                !named(&ui, affordance).is_empty(),
                "{screen} does not offer {affordance:?} — the two screens are \
                 back to holding opposite conventions, which is issue #18",
            );
        }

        ui.hide().expect("hide");
    }
}

/// The wording somebody reads, as well as the wording a screen reader hears.
///
/// The accessible label and the words on the card are two different strings and
/// only one of them is asserted above. Opening is the action with a consequence
/// outside this window, so it is the one that has to be legible without a
/// screen reader running — an unlabelled glyph for it would satisfy every
/// assertion in the test above.
#[test]
fn opening_is_offered_in_words_and_not_only_as_a_glyph() {
    i_slint_backend_testing::init_no_event_loop();

    for (screen, _, seed) in SCREENS {
        let ui = showing(*seed);

        let said: Vec<String> = ElementQuery::from_root(&ui)
            .match_descendants()
            .find_all()
            .iter()
            .filter_map(|element| {
                element
                    .accessible_label()
                    .or_else(|| element.accessible_value())
            })
            .map(|text| text.to_string())
            .collect();

        assert!(
            said.iter().any(|text| text == "Open in the block explorer"),
            "{screen} offers opening without saying so in words",
        );

        ui.hide().expect("hide");
    }
}

/// The link is on screen, not only behind the word "open".
///
/// This is the half of the sheet's old position that survives: somebody about
/// to hand a transaction id to a third party can read which host they are about
/// to tell. It was true on the sheet and false on the send screens, and issue
/// #18 argues it matters more on the send screens — so it is asserted on all of
/// them.
#[test]
fn the_host_a_transaction_would_be_shown_to_is_on_screen() {
    i_slint_backend_testing::init_no_event_loop();

    for (screen, url, seed) in SCREENS {
        let ui = showing(*seed);

        let shown = ElementQuery::from_root(&ui)
            .match_descendants()
            .find_all()
            .iter()
            .filter_map(ElementHandle::accessible_value)
            .any(|value| value == *url);

        assert!(
            shown,
            "{screen} does not show {url} anywhere — the destination is behind \
             a word, and the copy icon beside it acts on something invisible",
        );

        ui.hide().expect("hide");
    }
}

/// Pressing "open" asks for the explorer link, and for nothing else.
///
/// The one assertion here that is about safety rather than about consistency.
/// `Actions.open-link` is the single route out of this application to a
/// process, and `pecu-app` refuses anything `is_openable` does not recognise —
/// but that check can only defend a URL it is actually handed. This is what
/// pins that the URL handed over is the one the core built.
#[test]
fn pressing_open_hands_over_the_explorer_link_itself() {
    i_slint_backend_testing::init_no_event_loop();

    for (screen, url, seed) in SCREENS {
        let ui = showing(*seed);

        let asked = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        {
            let asked = asked.clone();
            ui.global::<Actions>()
                .on_open_link(move |link| asked.borrow_mut().push(link.to_string()));
        }

        named(&ui, "Open this transaction in the block explorer")
            .pop()
            .unwrap_or_else(|| panic!("{screen} offers opening"))
            .invoke_accessible_default_action();

        assert_eq!(
            asked.borrow().as_slice(),
            [(*url).to_string()],
            "{screen} asked to open something other than its own explorer link",
        );

        ui.hide().expect("hide");
    }
}
