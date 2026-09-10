//! What this wallet says about the money it holds and cannot send.
//!
//! # Why this is a test and not a screenshot
//!
//! Most of what the signpost has to get right is invisible in a picture. A
//! reference image proves the sentence is drawn; it cannot prove that pressing
//! the Bridge.vETH row asks Convert for Bridge.vETH rather than for whichever
//! currency the form was last left holding, and it cannot prove that the
//! sentence changes when the chain has conversions switched off — or, harder,
//! when nobody has managed to ask it. All of those are the difference between
//! a signpost and a wrong signpost, and a wrong one is worse than the silence
//! this replaces.
//!
//! Driven through the accessibility default action, like `tests/reveal.rs` — it
//! is the way a button is pressed from a test, and it runs the same `clicked`
//! handler a pointer does.

#![allow(clippy::expect_used, clippy::panic)]

mod support;

use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
use pecu_ui::{AppWindow, AssetRow, ConvertState, HaltState, Note, WalletState};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::cell::RefCell;
use std::rc::Rc;
use support::window_to_read;

/// The i-address the `funded` fixture gives Bridge.vETH.
const BRIDGE_VETH: &str = "iBoaN7swKAwXgYf1huA3PxBXi5stcfgGMh";

/// A second holding under the same name, and it is not a typo.
///
/// Two currencies on this chain can share a name component, which is the
/// recorded reason the shortcut carries an i-address and never a name. The
/// fixture holds one token, so a test that pressed one row and read one address
/// back would pass just as happily against a handler wired to a constant.
const OTHER_VETH: &str = "iCtawpxUiCc2sEupt7Z4u8SDAncGZpgSKm";

/// How a token row announces itself.
///
/// The name alone would not do: with two of them called Bridge.vETH, a label
/// that dropped the id would announce two different currencies as the same
/// button and leave a screen reader no way to tell the rows apart.
fn convert_action(name: &str, id: &str) -> String {
    format!("Convert {name}, {id}")
}

/// The half of each caption that asserts this chain is taking conversions.
///
/// The dashboard's and the send form's, because the same claim is made in two
/// wordings on two screens and the failure worth catching is one of them being
/// taught about a halt and the other not. Only one is ever on screen, so the
/// check is that neither is.
const ONLY_WHEN_CONVERSIONS_WORK: [&str; 2] = [
    "which says whether this one has a route",
    "converting it on the Convert screen",
];

/// Whether anything on screen claims conversions are working.
fn claims_conversions_work(said: &[String]) -> bool {
    said.iter().any(|text| {
        ONLY_WHEN_CONVERSIONS_WORK
            .iter()
            .any(|claim| text.contains(claim))
    })
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

/// A funded wallet on one of its screens, shown.
fn on(screen: &str) -> AppWindow {
    let ui = unlocked();
    pecu_ui::fixtures::funded(&ui);
    ui.set_screen(screen.into());
    ui.show().expect("show");
    ui
}

/// The same wallet, holding a second token under the first one's name.
///
/// Appended from the test rather than added to the fixture: no reference image
/// moves, and the fixture stays a picture of one wallet rather than one bent to
/// suit an assertion.
fn with_a_second_token(ui: &AppWindow) {
    let wallet = ui.global::<WalletState>();
    let mut assets: Vec<AssetRow> = wallet.get_assets().iter().collect();
    assets.push(AssetRow {
        name: "Bridge.vETH".into(),
        amount: "1.0000 0000".into(),
        secondary: OTHER_VETH.into(),
        currency_id: OTHER_VETH.into(),
        native: false,
        // A token row never folds in the shielded pool: there is one pool and
        // it holds the chain's own currency.
        counts_shielded: false,
    });
    wallet.set_assets(ModelRc::new(VecModel::from(assets)));
}

/// The control announcing itself by this name, if there is one.
fn named(ui: &AppWindow, label: &str) -> Option<ElementHandle> {
    ElementQuery::from_root(ui)
        .match_descendants()
        .find_all()
        .into_iter()
        .find(|element| element.accessible_label().as_deref() == Some(label))
}

/// Every piece of text on the window, label or not.
///
/// The captions this file is about are plain `Text`, which `accessible_label`
/// alone does not reach — the same reason `tests/translation.rs` reads both.
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

/// The one that matters: the row sends its own currency, not the list's.
///
/// The analogue of `reveal.rs::the_button_on_a_row_asks_for_that_rows_key`, and
/// it loops over two rows for the same reason that one does. A shortcut wired
/// to a constant, or to `assets[1]`, satisfies a one-token wallet and puts the
/// wrong currency in the pay field of the next one.
#[test]
fn pressing_a_token_row_opens_convert_with_that_currency_to_pay() {
    let _turn = window_to_read();

    for id in [BRIDGE_VETH, OTHER_VETH] {
        let ui = unlocked();
        pecu_ui::fixtures::funded(&ui);
        with_a_second_token(&ui);
        ui.set_screen("dashboard".into());
        ui.show().expect("show");

        named(&ui, &convert_action("Bridge.vETH", id))
            .expect("a Bridge.vETH row offering a conversion")
            .invoke_accessible_default_action();

        assert_eq!(
            ui.get_screen(),
            "convert",
            "pressing a token row did not open Convert",
        );
        assert_eq!(
            ui.global::<ConvertState>().get_from_address(),
            id,
            "Convert was opened to pay a currency other than the row that was pressed",
        );
        ui.hide().expect("hide");
    }
}

/// The chain's own row states a balance; it does not propose anything.
///
/// Converting coin is a thing Convert will happily do, and it is not what an
/// asset row is for. A list in which every line is also a button has stopped
/// telling the reader which of them is an offer.
#[test]
fn the_row_for_the_chains_own_currency_offers_no_conversion() {
    let _turn = window_to_read();

    let ui = on("dashboard");
    assert!(
        !ElementQuery::from_root(&ui)
            .match_descendants()
            .find_all()
            .iter()
            .filter_map(ElementHandle::accessible_label)
            .any(|label| label.starts_with("Convert VRSCTEST")),
        "the chain's own asset row is offering to convert itself",
    );

    // By role as well as by name: the title inside the row carries the same
    // word, so a lookup by name alone would have been asking a `Text` whether
    // it is pressable and getting the answer it likes.
    let row = ElementQuery::from_root(&ui)
        .match_descendants()
        .find_all()
        .into_iter()
        .find(|element| {
            element.accessible_role() == Some(AccessibleRole::Button)
                && element.accessible_label().as_deref() == Some("VRSCTEST")
        })
        .expect("the chain's own currency has a row");
    assert_eq!(
        row.accessible_enabled(),
        Some(false),
        "the chain's own asset row announces itself as pressable",
    );
    ui.hide().expect("hide");
}

/// A figure typed against one currency does not follow it into another.
///
/// "48.5" means one thing beside Bridge.vETH and something else entirely in a
/// form now paying in coin. Carrying it over would be the interface filling in
/// an amount nobody typed for the currency it is now about.
#[test]
fn an_amount_typed_for_one_currency_does_not_follow_it_to_another() {
    let _turn = window_to_read();

    let ui = on("dashboard");
    ui.global::<ConvertState>().set_pay_draft("250".into());

    named(&ui, &convert_action("Bridge.vETH", BRIDGE_VETH))
        .expect("the Bridge.vETH row offers a conversion")
        .invoke_accessible_default_action();

    assert_eq!(
        ui.global::<ConvertState>().get_pay_draft(),
        "",
        "an amount typed for another currency was carried into this one",
    );
    ui.hide().expect("hide");
}

/// The shortcut never builds a conversion of something into itself.
#[test]
fn the_shortcut_never_builds_a_conversion_into_itself() {
    let _turn = window_to_read();

    let ui = on("dashboard");
    // The form was last left buying the very token whose row is about to be
    // pressed — which is exactly what happens to somebody who looked up what
    // Bridge.vETH costs and then went back to the dashboard.
    ui.global::<ConvertState>().set_to_address(BRIDGE_VETH.into());

    named(&ui, &convert_action("Bridge.vETH", BRIDGE_VETH))
        .expect("the Bridge.vETH row offers a conversion")
        .invoke_accessible_default_action();

    let convert = ui.global::<ConvertState>();
    assert_eq!(
        convert.get_from_address(),
        BRIDGE_VETH,
        "the row did not become the leg being paid with",
    );
    assert_ne!(
        convert.get_to_address(),
        convert.get_from_address(),
        "the shortcut left a form converting a currency into itself",
    );
    ui.hide().expect("hide");
}

/// The send form says which currency it sends.
#[test]
fn the_send_form_says_which_currency_it_sends() {
    let _turn = window_to_read();

    let ui = unlocked();
    pecu_ui::fixtures::sending(&ui);
    ui.set_screen("send".into());
    ui.show().expect("show");

    assert!(
        texts(&ui)
            .iter()
            .any(|text| text.starts_with("This form sends VRSCTEST.")),
        "the send form never names the currency it is about",
    );
    ui.hide().expect("hide");
}

/// Neither sentence appears on a wallet holding only the chain's own currency.
///
/// The cost of getting this wrong is not a wrong sentence, it is a true one
/// nobody needed: a warning about tokens on the screen of somebody who has
/// none, which is how a wallet teaches people to stop reading its captions.
#[test]
fn a_wallet_holding_only_the_chains_own_currency_is_told_nothing_about_tokens() {
    let _turn = window_to_read();

    for screen in ["dashboard", "send"] {
        let ui = unlocked();
        pecu_ui::fixtures::sending(&ui);
        // The same wallet with the token taken out of it, rather than a second
        // fixture: what is under test is the flag, and a fixture that differed
        // in anything else would not say which difference mattered.
        ui.global::<WalletState>().set_holds_tokens(false);
        ui.set_screen(screen.into());
        ui.show().expect("show");

        assert!(
            !texts(&ui)
                .iter()
                .any(|text| text.contains("can be sent") || text.contains("This form sends")),
            "{screen} is telling a wallet with no tokens about tokens",
        );
        ui.hide().expect("hide");
    }
}

/// A chain with conversions switched off is not offered as a way out.
///
/// VRSCTEST has had `disabledefi` in force since block 1 187 000, so the
/// ordinary sentence — its row opens Convert, which says whether this one has
/// a route — describes a screen that will refuse every pair. Replacing a
/// silent absence with a confident wrong signpost is the worse of the two.
#[test]
fn a_chain_with_conversions_switched_off_does_not_offer_a_conversion_as_the_way_out() {
    let _turn = window_to_read();

    let ui = on("dashboard");
    ui.global::<HaltState>().set_conversions_halted(true);

    let said = texts(&ui);
    assert!(
        said.iter().any(|text| text
            .contains("this chain has conversions switched off")
            && text.contains("cannot be moved at all")),
        "the dashboard does not say that conversions are off",
    );
    assert!(
        !claims_conversions_work(&said),
        "the dashboard is telling somebody to convert on a chain that refuses every conversion",
    );
    // The sentence still names where the row goes. It is the only thing on the
    // card saying the row is pressable at all, and this is the state somebody
    // is most likely to press it in.
    assert!(
        said.iter().any(|text| text.contains("Its row opens Convert")),
        "the halted caption stopped saying where the row goes",
    );
    ui.hide().expect("hide");
}

/// And neither does the send form, which names the same door.
///
/// A separate test rather than a second loop in the one above, because the two
/// sentences are written in two files and the failure that matters is one of
/// them being updated and the other not.
#[test]
fn the_send_form_does_not_send_somebody_to_convert_while_it_is_shut() {
    let _turn = window_to_read();

    let ui = unlocked();
    pecu_ui::fixtures::sending(&ui);
    ui.global::<HaltState>().set_conversions_halted(true);
    ui.set_screen("send".into());
    ui.show().expect("show");

    let said = texts(&ui);
    assert!(
        said.iter()
            .any(|text| text.starts_with("This form sends VRSCTEST.")
                && text.contains("conversions switched off")),
        "the send form does not say that conversions are off",
    );
    assert!(
        !claims_conversions_work(&said),
        "the send form is pointing at Convert on a chain that refuses every conversion",
    );
    ui.hide().expect("hide");
}

/// A chain nobody could ask is not reported as one that is taking conversions.
///
/// The state the core refuses to guess about. `upgrade::Status::unknown` leaves
/// `conversions_halted` false because the field has to hold something, and a
/// caption reading that bare boolean prints the instruction — on VRSCTEST,
/// where `disabledefi` has stood since block 1 187 000, a false one. The empty
/// severity is the same hazard arriving earlier: the dashboard is the first
/// screen after unlock and the portfolio can land before the halt does.
#[test]
fn a_chain_this_wallet_could_not_ask_is_not_reported_as_taking_conversions() {
    let _turn = window_to_read();

    for severity in ["", "unknown"] {
        // Both screens in one loop: the two sentences live in two files, and
        // the failure worth catching is one of them learning this and the
        // other not.
        for screen in ["dashboard", "send"] {
            let ui = unlocked();
            pecu_ui::fixtures::sending(&ui);
            let halt = ui.global::<HaltState>();
            halt.set_severity(severity.into());
            halt.set_conversions_halted(false);
            ui.set_screen(screen.into());
            ui.show().expect("show");

            let said = texts(&ui);
            assert!(
                said.iter().any(|text| text.contains("does not know whether")),
                "{screen} does not say that the halt could not be read (severity {severity:?})",
            );
            assert!(
                !claims_conversions_work(&said),
                "{screen} is instructing a conversion on a chain nobody asked (severity {severity:?})",
            );
            ui.hide().expect("hide");
        }
    }
}

/// The row hands back a form, whatever Convert was left showing.
///
/// Nothing resets `ConvertState.step` on navigation, so a review or a receipt
/// left on screen is still what this row would open — with the currency swapped
/// underneath it and a fresh quote already sent. The two exits are wired in
/// `pecu-app`, which this crate cannot see, so they are stood in for here: what
/// is asserted is that the handler goes *through* them rather than writing the
/// step itself, because the review exit is also what tells the core to drop the
/// signed bytes it is holding.
#[test]
fn the_row_hands_back_a_form_whatever_convert_was_left_showing() {
    let _turn = window_to_read();

    for (left_on, expected) in [
        ("review", "cancel"),
        ("sent", "done"),
        ("uncertain", "done"),
    ] {
        let ui = on("dashboard");
        let taken = Rc::new(RefCell::new(String::new()));
        // What `main.rs` does on each exit, as far as this file can see it:
        // name itself, and put the screen back on its form.
        let record = |exit: &'static str| {
            let taken = Rc::clone(&taken);
            let weak = ui.as_weak();
            move || {
                taken.borrow_mut().push_str(exit);
                if let Some(ui) = weak.upgrade() {
                    ui.global::<ConvertState>().set_step("form".into());
                }
            }
        };
        let convert = ui.global::<ConvertState>();
        convert.on_cancel(record("cancel"));
        convert.on_done(record("done"));
        convert.set_step(left_on.into());

        named(&ui, &convert_action("Bridge.vETH", BRIDGE_VETH))
            .expect("the Bridge.vETH row offers a conversion")
            .invoke_accessible_default_action();

        assert_eq!(
            taken.borrow().as_str(),
            expected,
            "the row left on {left_on} did not leave through the screen's own exit",
        );
        assert_eq!(
            ui.global::<ConvertState>().get_step(),
            "form",
            "the row opened Convert on {left_on} rather than on its form",
        );
        ui.hide().expect("hide");
    }
}

/// A refusal from earlier is not waiting on the form this row opens.
///
/// `problem` is drawn in red under the build button, and nothing on the way in
/// clears it. Without this, pressing Bridge.vETH lands on a clean pre-filled
/// form carrying "This chain is not taking conversions", or a not-enough-held
/// line about a currency nobody just picked.
#[test]
fn a_refusal_from_earlier_is_not_waiting_on_the_form_the_row_opens() {
    let _turn = window_to_read();

    let ui = on("dashboard");
    ui.global::<ConvertState>().set_problem(Note {
        code: "halt-conversions".into(),
        ..Default::default()
    });

    named(&ui, &convert_action("Bridge.vETH", BRIDGE_VETH))
        .expect("the Bridge.vETH row offers a conversion")
        .invoke_accessible_default_action();

    assert_eq!(
        ui.global::<ConvertState>().get_problem().code,
        "",
        "the form the row opened is still showing a refusal about something else",
    );
    ui.hide().expect("hide");
}
