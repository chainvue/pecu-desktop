//! Which key the wallet is about to show the words for.
//!
//! # Why this is a test and not a screenshot
//!
//! The keys screen offers "Show recovery phrase" on every row that has one, and
//! the rows are identical apart from the name on them. A reference image proves
//! the button is drawn; it cannot prove that the one on the `savings` row asks
//! for `savings`. That is the whole of the bug this route was added to fix —
//! the previous caller read the label out of `WalletState.backup-key`, so every
//! row would have shown the same key's phrase, and a picture of three correct
//! buttons is exactly what that failure looks like.
//!
//! The consequence of getting it wrong is not a wrong screen. It is 24 words
//! copied onto a sheet of paper labelled with a key they do not restore, found
//! years later by somebody who has nothing else left.
//!
//! Driven through the accessibility default action, like `tests/accessibility.rs`
//! — it is the only way to press a button from a test, and it presses the same
//! `clicked` handler a pointer does.

#![allow(clippy::expect_used, clippy::panic)]

mod support;

use i_slint_backend_testing::{ElementHandle, ElementQuery};
use pecu_ui::{AppWindow, SeedState, WalletState};
use slint::ComponentHandle;
use support::window_to_read;

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

/// Every element the accessibility tree offers, with its name.
fn named(ui: &AppWindow, label: &str) -> Vec<ElementHandle> {
    ElementQuery::from_root(ui)
        .match_descendants()
        .find_all()
        .into_iter()
        .filter(|element| element.accessible_label().as_deref() == Some(label))
        .collect()
}

/// The keys screen sends the row somebody pressed, not the key the wallet
/// happens to be nagging about.
///
/// A window per row rather than two presses into one: pressing the first one
/// replaces the whole shell with the backup screen, so the second row is no
/// longer on screen to press. Which is the correct behaviour and an awkward
/// test, and the awkwardness is worth less than the coverage.
#[test]
fn the_button_on_a_row_asks_for_that_rows_key() {
    let _turn = window_to_read();

    // Three keys in the fixture: `main`, backed up; `savings`, generated and
    // never written down; `cold-storage`, a WIF import.
    //
    // `re-reading` is the other half of what the row has to carry: it decides
    // whether the sitting ends in the verification step or on a Done button,
    // and it is a property of the row, not of the wallet.
    for (label, re_reading) in [("main", true), ("savings", false)] {
        let ui = unlocked();
        pecu_ui::fixtures::keys(&ui);
        ui.set_screen("settings".into());
        ui.show().expect("show");

        let button = named(&ui, &format!("Show recovery phrase for {label}"))
            .pop()
            .unwrap_or_else(|| panic!("the {label} row offers its phrase"));
        button.invoke_accessible_default_action();

        let seed = ui.global::<SeedState>();
        assert_eq!(
            seed.get_label(),
            label,
            "the button on the {label} row asked for a different key",
        );
        assert_eq!(
            seed.get_step(),
            "passphrase",
            "the passphrase gate was skipped for {label}",
        );
        assert_eq!(
            seed.get_re_reading(),
            re_reading,
            "the {label} row got the wrong end for its sitting",
        );

        ui.hide().expect("hide");
    }
}

/// A key that arrived as a private key is not offered words it never had.
///
/// Refused by not offering, rather than by offering and failing — so the check
/// is that nothing on that row can be pressed at all. The row says "no recovery
/// phrase" instead, two elements to the left.
#[test]
fn a_key_imported_as_a_private_key_is_not_offered_a_phrase() {
    let _turn = window_to_read();

    let ui = unlocked();
    pecu_ui::fixtures::keys(&ui);
    ui.set_screen("settings".into());
    ui.show().expect("show");

    assert!(
        named(&ui, "Show recovery phrase for cold-storage").is_empty(),
        "a WIF import was offered a recovery phrase it has never had",
    );
    // The two that do have one are still there, so an empty result above is a
    // refusal and not a screen that failed to draw. Presence rather than a
    // count: one control answers to its name more than once here, and a number
    // would be a test about Slint's accessibility tree rather than about the
    // keys screen.
    for label in ["main", "savings"] {
        assert!(
            !named(&ui, &format!("Show recovery phrase for {label}")).is_empty(),
            "the keys screen did not draw the action on the {label} row",
        );
    }

    ui.hide().expect("hide");
}

/// Reading words again names the key, and asks nothing at the end of it.
///
/// Both halves are the reason the route exists. The name is what makes the
/// sheet of paper labellable — and what gives somebody who pressed the wrong
/// row a chance to notice before they copy anything down. The single Done
/// button is what says a re-read claims nothing: a quiz about words that are on
/// the screen in front of somebody proves nothing to anybody, and answering it
/// would be the wallet asking them to re-earn a backup they already made.
#[test]
fn a_re_read_names_its_key_and_ends_without_a_quiz() {
    let _turn = window_to_read();

    let ui = unlocked();
    pecu_ui::fixtures::backup_reread(&ui);
    ui.show().expect("show");

    assert!(
        !named(&ui, "The recovery phrase for main").is_empty(),
        "the phrase screen did not say which key the words belong to",
    );
    assert!(
        !named(&ui, "Done").is_empty(),
        "a re-read had no way out of the screen",
    );
    assert!(
        named(&ui, "I've written it down").is_empty(),
        "a re-read asked somebody to confirm a backup they had already made",
    );

    ui.hide().expect("hide");
}

/// And a first backup still ends in the verification it always did.
///
/// The other half of the test above: `ends-here` decides between two ways off
/// this screen, and a change that pointed it the wrong way would take the quiz
/// out of the one sitting that is worth quizzing.
#[test]
fn a_first_backup_still_has_to_be_written_down() {
    let _turn = window_to_read();

    let ui = unlocked();
    pecu_ui::fixtures::backup_phrase(&ui);
    ui.show().expect("show");

    assert!(
        !named(&ui, "I've written it down").is_empty(),
        "a first backup no longer asks for the words back",
    );
    assert!(
        named(&ui, "Done").is_empty(),
        "a first backup offered a way out that records nothing",
    );

    ui.hide().expect("hide");
}

/// The passphrase gate names the key too, which is the last moment to notice.
///
/// The words come out on the next screen. Everything up to here is undone by
/// pressing Escape; after it there is a phrase on a screen and possibly on
/// paper. So the name has to be on this screen and not only on the one after
/// it.
#[test]
fn the_gate_in_front_of_the_words_names_the_key() {
    let _turn = window_to_read();

    let ui = unlocked();
    pecu_ui::fixtures::backup_passphrase(&ui);
    ui.show().expect("show");

    assert!(
        !named(&ui, "Show the recovery phrase for main").is_empty(),
        "the passphrase prompt did not say whose phrase it is about to show",
    );
    assert!(
        named(&ui, "Show your recovery phrase").is_empty(),
        "the passphrase prompt fell back to the nameless heading",
    );

    ui.hide().expect("hide");
}
