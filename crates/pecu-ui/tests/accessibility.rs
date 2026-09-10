//! Nothing in this interface is nameless.
//!
//! # What this can check, and what it cannot
//!
//! A screen reader announces an element by its role and its label. An element
//! with a role and no label is announced as "button" — nothing else — and a
//! screen with four of those is four identical buttons to somebody who cannot
//! see which is which. That failure is mechanical, it is invisible in review,
//! and it is what this test is for.
//!
//! What it **cannot** check is whether the result makes sense heard: whether the
//! order is followed, whether the wording is right when it arrives as sound
//! rather than as text beside an icon, whether a change is announced at a
//! useful moment. That needs a person with VoiceOver, and it is on the parked
//! list rather than pretended at here.
//!
//! So: this is the half that can be automated, asserted honestly as half.
//!
//! # Why the labels are read off live elements
//!
//! Grepping the `.slint` source would pass on a label attached to something
//! that never renders, and fail on one produced by a binding. These come from
//! Slint's own element-inspection backend, walking the tree the compiler built.

#![allow(clippy::expect_used, clippy::panic)]

mod support;

use pecu_ui::{AppWindow, WalletState};
use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
use slint::ComponentHandle;
use support::window_to_read;

/// Roles that a person operates, and must therefore be able to tell apart.
///
/// Deliberately not every role. `text` carries its own content as its name, and
/// requiring a separate label on every piece of static text would produce
/// labels nobody reads and a test nobody trusts.
fn is_interactive(role: AccessibleRole) -> bool {
    matches!(
        role,
        AccessibleRole::Button
            | AccessibleRole::Checkbox
            | AccessibleRole::Combobox
            | AccessibleRole::Slider
            | AccessibleRole::Spinbox
            | AccessibleRole::Switch
            | AccessibleRole::Tab
            | AccessibleRole::TextInput
    )
}

/// Slint's own text-entry primitive, which cannot be given a label directly.
///
/// It always reports the `text-input` role and carries no name of its own; the
/// name comes from whatever wraps it. In this interface that wrapper is always
/// `Field`, and [`no_text_input_is_left_without_a_name`] is what
/// makes skipping these safe rather than convenient — without that check, this
/// exclusion would also hide a bare unlabelled input somebody added directly.
const SLINT_TEXT_PRIMITIVE: &str = "TextInput";

/// How an element is identified in a failure, best effort.
fn describe(element: &ElementHandle) -> String {
    let kind = element
        .type_name()
        .map_or_else(|| "?".to_string(), |name| name.to_string());
    let id = element
        .id()
        .map_or_else(String::new, |id| format!(" #{id}"));
    format!("{kind}{id}")
}

/// Set the wallet up so the shell is on screen rather than the unlock form.
fn unlocked() -> AppWindow {
    let ui = AppWindow::new().expect("a window");
    pecu_ui::chart::install(&ui);
    let wallet = ui.global::<WalletState>();
    wallet.set_loading(false);
    wallet.set_exists(true);
    wallet.set_locked(false);
    ui
}

/// Every screen, seeded the way its reference image is.
///
/// The seeded states matter: a control only reachable once there is a balance,
/// or once a review has been built, is exactly the kind that ships nameless
/// because nobody looked at it with a screen reader running.
type Screen = (&'static str, fn(&AppWindow));

const SCREENS: &[Screen] = &[
    ("dashboard", pecu_ui::fixtures::funded),
    ("markets", pecu_ui::fixtures::markets),
    // The detail is its own view now, and the breadcrumb that leaves it is the
    // only way back other than Escape. A screen reader that cannot find it is a
    // screen reader with no way out.
    ("markets", pecu_ui::fixtures::market_detail),
    ("markets", pecu_ui::fixtures::markets_empty),
    ("convert", pecu_ui::fixtures::converting),
    ("convert", pecu_ui::fixtures::converting_refused),
    // The refused review, which has its own button row rather than the one
    // every other state of this card shares — so the walk above was not
    // reaching it, and the row it does reach proves nothing about this one.
    ("convert", pecu_ui::fixtures::converting_rejected),
    ("activity", pecu_ui::fixtures::history),
    // The three screens that end with a transaction id, added with #18 — which
    // gave each of them a second control beside the first and is therefore
    // exactly the change this test exists to catch. None of the three had ever
    // been walked here, so the copy button on a sent payment had gone unchecked
    // since it was written, and the sheet's read-only fields announced
    // themselves as "text field" and nothing else.
    ("send", pecu_ui::fixtures::sent),
    ("convert", pecu_ui::fixtures::converting_sent),
    ("activity", pecu_ui::fixtures::tx_detail),
    ("send", pecu_ui::fixtures::sending),
    ("send", pecu_ui::fixtures::sending_too_much),
    // Listed for completeness, not for coverage: every control on this state is
    // one the `sending` entry above already walks, and the "Send everything"
    // toggle is a `Btn` like any other, so it is named either way.
    //
    // What is genuinely new here is the thing this test cannot see. The toggle
    // carries its on/off state in `variant: primary/secondary` — colour — and
    // `Btn` announces `accessible-label: root.text` and nothing else, so a
    // screen reader says "Send everything, button" whether the mode is on or
    // off. `accessible-checked` appears nowhere in this tree; the pool selector
    // beside it carries its selection exactly the same way. So this is a gap in
    // the convention rather than in this screen, and closing it means teaching
    // `Btn` about a checked state everywhere at once.
    ("send", pecu_ui::fixtures::sending_everything),
    ("nodes", pecu_ui::fixtures::network_trouble),
    ("send", pecu_ui::fixtures::reviewing),
    ("receive", pecu_ui::fixtures::receiving),
    ("activity", pecu_ui::fixtures::funded),
    ("settings", pecu_ui::fixtures::settings),
    ("settings", pecu_ui::fixtures::keys),
    // The backup screen, in both states it can end in. It replaces the shell
    // rather than sitting on a screen, so the name beside it is only the one
    // the reference images file it under. It is the highest-consequence screen
    // in the wallet and had no entry here at all — including the "Done" that is
    // the only way out of a re-read, which is the one control on it somebody
    // driving by screen reader has to be able to find.
    ("dashboard", pecu_ui::fixtures::backup_passphrase),
    ("dashboard", pecu_ui::fixtures::backup_reread),
    ("settings", pecu_ui::fixtures::addresses),
    ("settings", pecu_ui::fixtures::general_settings),
    ("nodes", pecu_ui::fixtures::network),
    ("identities", pecu_ui::fixtures::identities),
    ("identities", pecu_ui::fixtures::claiming_a_name),
    ("identities", pecu_ui::fixtures::claiming_authority),
    ("identities", pecu_ui::fixtures::claiming_review),
    ("identities", pecu_ui::fixtures::identity_detail),
    ("identities", pecu_ui::fixtures::identity_authorities),
    ("currencies", pecu_ui::fixtures::currencies),
    ("currencies", pecu_ui::fixtures::currency_kind),
    ("currencies", pecu_ui::fixtures::currency_identity),
    ("currencies", pecu_ui::fixtures::defining_currency),
    ("currencies", pecu_ui::fixtures::currency_nft),
    ("currencies", pecu_ui::fixtures::currency_reserve_picker),
    ("currencies", pecu_ui::fixtures::currency_review),
    ("currencies", pecu_ui::fixtures::currency_authority),
    ("currencies", pecu_ui::fixtures::launching_currency),
    (
        "currencies",
        pecu_ui::fixtures::claiming_a_name_for_a_currency,
    ),
    ("currencies", pecu_ui::fixtures::launch_pending),
    ("currencies", pecu_ui::fixtures::currencies_empty),
    ("identities", pecu_ui::fixtures::registering),
    ("identities", pecu_ui::fixtures::identity_change_review),
    ("identities", pecu_ui::fixtures::identity_revoke_review),
];

/// Walk every screen, handing each element with a role to `visit`.
fn each_control(mut visit: impl FnMut(&str, &ElementHandle, AccessibleRole)) {
    for (screen, seed) in SCREENS {
        let ui = unlocked();
        seed(&ui);
        ui.set_screen((*screen).into());
        ui.show().expect("show");

        for element in &ElementQuery::from_root(&ui).match_descendants().find_all() {
            if let Some(role) = element.accessible_role() {
                visit(screen, element, role);
            }
        }

        ui.hide().expect("hide");
    }
}

/// Announcing a button is half of it. Pressing it is the other half.
///
/// # Why a separate test, and why it is the more important one
///
/// A name tells VoiceOver a control is there. Activating it goes through the
/// **default action**, which is its own contract in Slint — and nothing in this
/// interface declared one. Every button in the wallet was announced correctly
/// and did nothing when a screen reader pressed it, which is a worse failure
/// than a nameless button: the nameless one is obviously broken.
///
/// The breadcrumb is a way out, not a label that looks like one.
///
/// The market detail is a view rather than an overlay: no scrim to click away,
/// and the row that opened it is no longer on screen. So there are exactly two
/// ways back — Escape, which `tests/shortcuts.rs` presses, and this. A
/// breadcrumb that announces itself and does nothing when activated would leave
/// somebody driving by screen reader with one route out and no sign that the
/// other was decoration.
#[test]
fn the_breadcrumb_out_of_a_market_can_be_pressed() {
    let _turn = window_to_read();

    let ui = unlocked();
    pecu_ui::fixtures::market_detail(&ui);
    ui.set_screen("markets".into());
    ui.show().expect("show");

    let asked = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
    {
        let asked = asked.clone();
        ui.global::<pecu_ui::MarketState>()
            .on_select(move |address| asked.borrow_mut().push(address.to_string()));
    }

    let back = ElementQuery::from_root(&ui)
        .match_descendants()
        .find_all()
        .into_iter()
        .find(|element| element.accessible_label().as_deref() == Some("Back to Markets"))
        .expect("the breadcrumb is the way out of a market detail");

    back.invoke_accessible_default_action();
    ui.hide().expect("hide");

    assert_eq!(
        asked.borrow().as_slice(),
        [String::new()],
        "pressing the breadcrumb the way a screen reader does did not ask to \
         close the market",
    );
}

/// Asserted here by pressing the interface's own primary control on the send
/// screen and checking the wallet was told. The named button is chosen rather
/// than "any button" because a test that pressed whatever it found first would
/// pass on something harmless while the important controls stayed inert.
#[test]
fn a_screen_reader_can_actually_press_a_button() {
    let _turn = window_to_read();

    let ui = unlocked();
    pecu_ui::fixtures::sending(&ui);
    ui.set_screen("send".into());
    ui.show().expect("show");

    let pressed = std::rc::Rc::new(std::cell::Cell::new(false));
    {
        let pressed = pressed.clone();
        ui.global::<pecu_ui::Actions>()
            .on_lock(move || pressed.set(true));
    }

    let lock = ElementQuery::from_root(&ui)
        .match_descendants()
        .find_all()
        .into_iter()
        .find(|element| element.accessible_label().as_deref() == Some("Lock the wallet"))
        .expect("the Lock control is on every screen of an unlocked wallet");

    lock.invoke_accessible_default_action();
    ui.hide().expect("hide");

    assert!(
        pressed.get(),
        "activating a button the way a screen reader does had no effect — the \
         control announces itself and cannot be operated",
    );
}

#[test]
fn every_control_a_screen_reader_can_reach_has_a_name() {
    let _turn = window_to_read();

    let mut nameless = Vec::new();
    let mut checked = 0usize;

    each_control(|screen, element, role| {
        if !is_interactive(role) {
            return;
        }
        // Slint's own primitive takes its name from its wrapper. The test below
        // is what keeps that from being a hole.
        if element.type_name().as_deref() == Some(SLINT_TEXT_PRIMITIVE) {
            return;
        }
        checked += 1;

        let named = element
            .accessible_label()
            .is_some_and(|label| !label.trim().is_empty())
            // A field is legitimately empty until somebody types in it, and
            // announces its placeholder instead. That is a name.
            || element
                .accessible_placeholder_text()
                .is_some_and(|hint| !hint.trim().is_empty());

        if !named {
            nameless.push(format!("{screen}: {} ({role:?})", describe(element)));
        }
    });

    assert!(
        checked > 20,
        "only {checked} interactive elements were found across seven screens — \
         the walk is not reaching the interface, so this test is passing on \
         nothing. Slint drops the element tree unless `build.rs` asks for debug \
         info; check that first.",
    );

    assert!(
        nameless.is_empty(),
        "{} control(s) a screen reader would announce with no name at all:\n  {}",
        nameless.len(),
        nameless.join("\n  "),
    );
}

/// No text input is left without something to name it.
///
/// This is the assumption the test above rests on. Slint's `TextInput` reports
/// the `text-input` role and has no name of its own, so a screen reader gets
/// "text field" and nothing more — unless something supplies one. There are
/// exactly two ways that happens here, and both are legitimate:
///
/// * wrapped in a `Field`, which sets the role, the label and the value
///   together — and deliberately withholds the value when the field is masked;
/// * carrying its own label, which is what a read-only `TextInput` used to
///   *display* something does. The receive address is one: it is a text input
///   only so the characters can be selected and copied, and it names itself and
///   speaks its value in groups.
///
/// Anything else is nameless, and the exclusion above would hide it.
#[test]
fn no_text_input_is_left_without_a_name() {
    let _turn = window_to_read();

    let mut bare = Vec::new();
    let mut inputs = 0usize;

    each_control(|screen, element, _role| {
        if element.type_name().as_deref() != Some(SLINT_TEXT_PRIMITIVE) {
            return;
        }
        inputs += 1;

        // `Field` gives its inner input this id.
        let wrapped = element.id().as_deref() == Some("Field::input");
        let names_itself = element
            .accessible_label()
            .is_some_and(|label| !label.trim().is_empty());

        if !wrapped && !names_itself {
            bare.push(format!("{screen}: {}", describe(element)));
        }
    });

    assert!(
        inputs > 0,
        "no text inputs were found at all, so this proves nothing",
    );
    assert!(
        bare.is_empty(),
        "{} text input(s) have no name a screen reader can read — wrap them in \
         a `Field`, or give them an `accessible-label` of their own:\n  {}",
        bare.len(),
        bare.join("\n  "),
    );
}

/// An address is announced in groups, not as sixty-four run-on characters.
///
/// VoiceOver reads raw base58 as an unbroken run of letters, which is unusable
/// for the one thing somebody would be listening for: checking that the address
/// on screen is the address they meant. Groups give it a rhythm that can be
/// followed and repeated back.
///
/// This is checked on Receive, where the whole point of the screen is an address
/// somebody is about to hand out.
#[test]
fn an_address_is_spoken_in_groups() {
    let _turn = window_to_read();

    let ui = unlocked();
    pecu_ui::fixtures::receiving(&ui);
    ui.set_screen("receive".into());
    ui.show().expect("show");

    let grouped = ElementQuery::from_root(&ui)
        .match_descendants()
        .find_all()
        .iter()
        .filter_map(ElementHandle::accessible_value)
        .find(|value| {
            // Four characters, a space, four characters — repeated. Enough of a
            // shape that a run-on address cannot match it.
            let chunks: Vec<&str> = value.split(' ').collect();
            chunks.len() >= 6 && chunks.iter().all(|chunk| chunk.len() <= 4)
        });

    assert!(
        grouped.is_some(),
        "no element on Receive exposes the address in groups — a screen reader \
         will read it as one unbroken run of base58",
    );

    ui.hide().expect("hide");
}
