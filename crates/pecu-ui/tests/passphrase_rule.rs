//! Every form that sets a passphrase says the rule, and none that opens one does.
//!
//! # What this is defending
//!
//! Two claims, and they pull in opposite directions.
//!
//! The first is the issue's own: a floor applied to one entry point does not
//! remove a weak passphrase, it moves it. Three forms ask somebody to choose
//! one — setting up, restoring, and Change passphrase — and all three have to
//! carry the same rule and the same words. A screenshot proves a sentence was
//! drawn on the screen that was photographed; it cannot prove the fourth form
//! has one, which is precisely the form that ships without it.
//!
//! The second is the one nobody would notice going wrong until somebody was
//! locked out. `PassphraseForm` is the same component in both modes: the wallet
//! that does not exist yet, and the wallet that does. A length rule that
//! reached the unlock mode would refuse a wallet sealed before the rule
//! existed, and there is no reset, no hint and no second copy. So the last
//! function here types six characters into the unlock box and requires the
//! button to be live.
//!
//! # Why it drives the accessibility tree
//!
//! Same reason as `tests/reveal.rs` and `tests/accessibility.rs`: it is the
//! only way to put text into a field and read a button's state from a test, and
//! it goes through the same properties an assistive technology reads. Which
//! makes it the honest place to assert the "Too short" line as well — the rule
//! must be legible to somebody who cannot see that the sentence went red.
//!
//! # One `#[test]`, several functions
//!
//! Slint permits one platform per process and the harness runs `#[test]`s on
//! separate threads, so this follows `tests/seed_words.rs`: one test, calling
//! the cases in sequence.

#![allow(clippy::expect_used, clippy::panic)]

use i_slint_backend_testing::{ElementHandle, ElementQuery};
use pecu_ui::{AppInfo, AppWindow};
use slint::ComponentHandle;

/// The passphrase from issue #16, which every one of these forms used to take.
const GUESSABLE: &str = "cat123";

/// Long enough, and the kind of thing the hint is actually recommending.
const GOOD: &str = "correct horse battery staple";

/// The wording the rule shows when what has been typed is under the floor.
const TOO_SHORT: &str = "Too short.";

fn floor() -> usize {
    pecu_protocol::MIN_PASSPHRASE_CHARS
}

/// Every accessible name on the window, deduplicated.
///
/// `find_all` reports an element once per type in its inheritance chain, so a
/// `Text` arrives two or three times. Nothing here counts occurrences, only
/// asks whether a sentence is present, so the duplicates are dropped rather
/// than reasoned about.
fn sentences(ui: &AppWindow) -> Vec<String> {
    let mut found: Vec<String> = ElementQuery::from_root(ui)
        .match_descendants()
        .find_all()
        .iter()
        .filter_map(ElementHandle::accessible_label)
        .map(|label| label.to_string())
        .collect();
    found.sort();
    found.dedup();
    found
}

fn says(ui: &AppWindow, fragment: &str) -> bool {
    sentences(ui).iter().any(|line| line.contains(fragment))
}

/// The text primitive inside the one `Field` whose placeholder is `placeholder`.
///
/// The placeholder is the field's accessible name — `Field` says so, and
/// `accessibility.rs` is what keeps that true — so it is the only handle a test
/// has on a box that is deliberately masked. The inner `TextInput` is what
/// accepts a value; the `Field` around it publishes an empty one on purpose,
/// because a password field that exports its contents to the accessibility tree
/// has undone the masking.
fn field(ui: &AppWindow, placeholder: &str) -> ElementHandle {
    let mut inputs: Vec<ElementHandle> = ElementQuery::from_root(ui)
        .match_descendants()
        .find_all()
        .into_iter()
        .filter(|element| {
            element.type_name().as_deref() == Some("Field")
                && element.accessible_label().as_deref() == Some(placeholder)
        })
        .flat_map(|found| {
            found
                .query_descendants()
                .match_type_name("TextInput")
                .find_all()
        })
        .collect();

    // One box, several handles: `find_all` reports an element once per type in
    // its inheritance chain, and each of those handles then finds the same
    // primitive again. Position is what tells two boxes apart from one box seen
    // five times, and nothing in these forms puts two fields in one place.
    inputs.dedup_by_key(|input| {
        let at = input.absolute_position();
        (at.x.to_bits(), at.y.to_bits())
    });

    assert_eq!(
        inputs.len(),
        1,
        "expected exactly one box labelled {placeholder:?}, found {}",
        inputs.len(),
    );

    inputs.remove(0)
}

/// Type into a masked box.
fn type_into(ui: &AppWindow, placeholder: &str, text: &str) {
    field(ui, placeholder).set_accessible_value(text);
}

/// Whether the button with this name can be pressed.
///
/// `Btn` publishes `accessible-enabled` from its own `enabled`, so this is the
/// same answer a pointer would get. Several handles come back for one button —
/// the component and the `Text` inside it — and only the component carries the
/// flag, so this reads the ones that have an answer and requires them to agree.
fn can_press(ui: &AppWindow, label: &str) -> bool {
    let states: Vec<bool> = ElementQuery::from_root(ui)
        .match_descendants()
        .find_all()
        .iter()
        .filter(|element| element.accessible_label().as_deref() == Some(label))
        .filter_map(ElementHandle::accessible_enabled)
        .collect();

    assert!(!states.is_empty(), "no button named {label:?} on screen");
    assert!(
        states.iter().all(|state| *state == states[0]),
        "two buttons named {label:?} disagree about being pressable",
    );
    states[0]
}

/// A window the size the reference images are taken at.
///
/// Sized explicitly rather than left at the default, because the element tree
/// reports what is **rendered** and these screens live in a `Flickable`: a
/// control scrolled past the bottom is missing from the tree rather than
/// present and disabled, which would make "the button is off" and "the button
/// is not drawn" the same answer. `snapshot::WIDTH` and `HEIGHT` rather than a
/// number chosen to make this pass — the question is whether these forms work
/// at the size they are photographed at, so a window invented here could
/// certify a layout nobody will ever see.
fn window() -> AppWindow {
    let ui = AppWindow::new().expect("a window");
    ui.window().set_size(slint::PhysicalSize::new(
        pecu_ui::snapshot::WIDTH,
        pecu_ui::snapshot::HEIGHT,
    ));
    ui
}

/// A window with no wallet on disk: the setup form.
fn setting_up() -> AppWindow {
    let ui = window();
    pecu_ui::fixtures::fresh(&ui);
    ui
}

#[test]
fn the_passphrase_rule_is_stated_wherever_one_is_chosen() {
    i_slint_backend_testing::init_no_event_loop();

    the_interface_states_the_rule_the_vault_applies();
    setting_up_refuses_a_short_passphrase();
    restoring_refuses_a_short_passphrase();
    changing_refuses_a_short_new_passphrase();
    unlocking_is_left_alone();
}

/// The number on screen is the number the vault enforces.
///
/// `AppInfo.min-passphrase-chars` has a default in `state.slint` rather than
/// only a value pushed from `pecu-app`, because these forms are rendered by the
/// snapshot suite with no application behind them. A default is a second copy
/// of a constant, and a second copy drifts — so this holds it against
/// `pecu_protocol::MIN_PASSPHRASE_CHARS`, which is the one the keystore reads.
/// Without this, raising the floor in the vault would leave three screens
/// quoting a rule nobody applies and switching their buttons on below it.
fn the_interface_states_the_rule_the_vault_applies() {
    let ui = setting_up();

    assert_eq!(
        usize::try_from(ui.global::<AppInfo>().get_min_passphrase_chars()).expect("not negative"),
        floor(),
        "the interface and the vault disagree about the shortest passphrase",
    );

    // And it is said out loud, not merely known. The sentence carries the
    // number, so this also catches a hint that hard-codes a stale one.
    assert!(
        says(&ui, &format!("At least {} characters", floor())),
        "the setup form does not state the length requirement:\n  {}",
        sentences(&ui).join("\n  "),
    );
}

/// `cat123` cannot be submitted, and the form says why before it is tried.
fn setting_up_refuses_a_short_passphrase() {
    let ui = setting_up();

    assert!(!can_press(&ui, "Create wallet"), "an empty form was live");
    assert!(!says(&ui, TOO_SHORT), "the form complained before anything was typed");

    // Both boxes agree, so the confirmation is not what is holding this back.
    type_into(&ui, "A passphrase you will remember", GUESSABLE);
    type_into(&ui, "Type it again", GUESSABLE);
    assert!(
        !can_press(&ui, "Create wallet"),
        "a six-character passphrase could be submitted",
    );
    assert!(
        says(&ui, TOO_SHORT),
        "nothing on screen said why Create wallet was dead:\n  {}",
        sentences(&ui).join("\n  "),
    );

    type_into(&ui, "A passphrase you will remember", GOOD);
    type_into(&ui, "Type it again", GOOD);
    assert!(can_press(&ui, "Create wallet"), "a good passphrase was refused");
    assert!(!says(&ui, TOO_SHORT), "the complaint outlived the problem");
}

/// The restore form seals a vault too, so it carries the same rule.
///
/// It is the easiest of the three to forget: the box people arrive worrying
/// about is the recovery phrase, and the passphrase is the third control down.
fn restoring_refuses_a_short_passphrase() {
    let ui = setting_up();
    ui.set_restoring(true);

    assert!(
        says(&ui, &format!("At least {} characters", floor())),
        "the restore form does not state the length requirement",
    );

    type_into(&ui, "abandon abandon abandon …", "abandon abandon abandon");
    type_into(&ui, "A passphrase you will remember", GUESSABLE);
    type_into(&ui, "Type it again", GUESSABLE);
    assert!(
        !can_press(&ui, "Restore wallet"),
        "a restore could be submitted with a six-character passphrase",
    );
    assert!(says(&ui, TOO_SHORT));

    type_into(&ui, "A passphrase you will remember", GOOD);
    type_into(&ui, "Type it again", GOOD);
    assert!(can_press(&ui, "Restore wallet"));
}

/// The form the issue is really about: without this, the weak passphrase moves
/// here instead of being refused.
fn changing_refuses_a_short_new_passphrase() {
    let ui = window();
    pecu_ui::fixtures::settings(&ui);

    assert!(
        says(&ui, &format!("At least {} characters", floor())),
        "Change passphrase does not state the length requirement",
    );

    // The current passphrase is whatever this wallet was sealed with, which
    // may well be shorter than the floor. It is not judged, and must not be:
    // see `unlocking_is_left_alone` below.
    type_into(&ui, "Passphrase", GUESSABLE);
    type_into(&ui, "A new passphrase", GUESSABLE);
    type_into(&ui, "Type it again", GUESSABLE);
    assert!(
        !can_press(&ui, "Change passphrase"),
        "a six-character new passphrase could be submitted",
    );
    assert!(says(&ui, TOO_SHORT));

    type_into(&ui, "A new passphrase", GOOD);
    type_into(&ui, "Type it again", GOOD);
    assert!(
        can_press(&ui, "Change passphrase"),
        "a short CURRENT passphrase blocked the change — this is the lockout",
    );
}

/// Nothing about the floor reaches the wallet that already exists.
///
/// This is the assertion that keeps the change safe to ship. The vault draws
/// the same line in the same place — the floor is in `Vault::create` and in the
/// new half of `Vault::change_passphrase`, and `derive`, which every unlock
/// runs, has never seen it — but the interface is the half somebody meets, and
/// a form that greys out Unlock is a lockout whatever the keystore would have
/// done. There is no way to re-check an existing passphrase without asking for
/// it, and asking is the moment this would refuse.
fn unlocking_is_left_alone() {
    let ui = window();
    pecu_ui::fixtures::locked(&ui);

    assert!(
        !says(&ui, &format!("At least {} characters", floor())),
        "the unlock form states a rule it does not apply",
    );

    type_into(&ui, "Passphrase", GUESSABLE);
    assert!(
        can_press(&ui, "Unlock"),
        "a wallet sealed with a short passphrase can no longer be opened",
    );
    assert!(!says(&ui, TOO_SHORT), "the unlock form complained about a length");
}
