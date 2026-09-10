//! The interface can be translated, demonstrated rather than asserted.
//!
//! # Why this test exists at all
//!
//! "It is ready for translation" is the kind of claim that stays true right up
//! until somebody tries it. There are four separate things that have to line
//! up — `@tr` in the source, the `.po` in the right directory, the build
//! bundling it, and the runtime switching to it — and three of them fail
//! silently. A missing `msgid` falls back to English, which is also exactly
//! what a working setup looks like on the English path.
//!
//! So this switches to German and reads the interface back out of the live
//! element tree, the same way `accessibility.rs` does. If the wiring breaks,
//! this goes red instead of the feature quietly not existing.
//!
//! # Why the German catalogue is partial
//!
//! Because it is a fixture, not a product. `translations/de/LC_MESSAGES/` has
//! the eight navigation labels and nothing else, which is enough to prove every
//! link in the chain. Shipping German is a separate decision and a translator's
//! job; see `translations/README.md`.

#![allow(clippy::expect_used, clippy::panic)]

mod support;

use i_slint_backend_testing::ElementQuery;
use pecu_ui::{AppWindow, Note, SendState, WalletState};
use slint::ComponentHandle;
use support::window_to_read;

/// The shell, on screen rather than behind the unlock form.
fn unlocked() -> AppWindow {
    let ui = AppWindow::new().expect("a window");
    let wallet = ui.global::<WalletState>();
    wallet.set_loading(false);
    wallet.set_exists(true);
    wallet.set_locked(false);
    ui
}

/// Every label a screen reader can reach on this window.
fn labels(ui: &AppWindow) -> Vec<String> {
    ElementQuery::from_root(ui)
        .match_descendants()
        .find_all()
        .iter()
        .filter_map(i_slint_backend_testing::ElementHandle::accessible_label)
        .map(|label| label.to_string())
        .collect()
}

/// Every piece of text on the window, label or not.
///
/// `accessible_label` misses a plain `Text`, and the notes on the send form are
/// plain `Text` — so a test that only read labels would have found nothing and
/// called it a pass.
fn texts(ui: &AppWindow) -> Vec<String> {
    ElementQuery::from_root(ui)
        .match_descendants()
        .find_all()
        .iter()
        .filter_map(|e| e.accessible_label().or_else(|| e.accessible_value()))
        .map(|t| t.to_string())
        .collect()
}

/// Selecting a language changes what the interface says, and an untranslated
/// string keeps its English.
///
/// # Why this is one test and not three
///
/// `select_bundled_translation` sets **process-global** state, and cargo runs
/// the tests in a file concurrently in one process. Split up, these raced: the
/// fallback test switched to German and the navigation test then found a German
/// interface before it had switched anything, which it correctly refused to
/// call a pass. A sequence that only means something in order belongs in one
/// test rather than in three that must not overlap.
#[test]
fn the_interface_can_be_switched_to_another_language() {
    let _turn = window_to_read();

    let ui = unlocked();

    // Explicitly, and this is the whole reason `main.rs` does it too: Slint
    // picks the bundled language from the **system locale** at startup, so on a
    // German machine this window comes up in German without being asked. That
    // is how it was found — this assertion failed on the developer's own
    // machine while passing everywhere the locale happened to be English.
    slint::select_bundled_translation("en").expect("the default language");

    let english = labels(&ui);
    assert!(
        english.iter().any(|l| l == "Dashboard"),
        "expected an English navigation before switching; got {english:?}"
    );

    // Must happen after a component exists — the bundle is attached to the
    // global context the first one creates.
    slint::select_bundled_translation("de").expect("the bundled German catalogue");

    let german = labels(&ui);
    // Four rail entries, chosen because they are on screen in the state this
    // test builds. `Network` used to be the fourth and is not a rail entry any
    // more — it is a tab inside Settings — so asserting it here asserted
    // against a string nothing renders, which is a test that fails for a reason
    // that has nothing to do with translation.
    for (from, to) in [
        ("Dashboard", "Übersicht"),
        ("Send", "Senden"),
        ("Receive", "Empfangen"),
        ("Settings", "Einstellungen"),
    ] {
        assert!(
            german.iter().any(|l| l == to),
            "expected {from:?} to become {to:?}; got {german:?}"
        );
        assert!(
            !german.iter().any(|l| l == from),
            "{from:?} is still English after switching; got {german:?}"
        );
    }

    // An untranslated string falls back to English rather than to nothing. This
    // is the failure mode worth pinning: a half-finished catalogue has to leave
    // the interface usable, not blank. "Refresh" is deliberately absent from
    // the German fixture.
    assert!(
        german.iter().any(|l| l == "Refresh"),
        "an untranslated label should stay English rather than vanish; got {german:?}"
    );

    // The screen title is read out of the same navigation entry as the rail,
    // rather than from a second list of the same eight words. It was a second
    // list until this test found it still saying "Dashboard" in a German
    // window.
    assert!(
        german.iter().filter(|l| *l == "Übersicht").count() >= 2,
        "the title bar should be translated with the rail; got {german:?}"
    );

    // A sentence that used to be written in Rust.
    //
    // This is the point of `NoteVm`. `pecu-core` decided *and worded* every
    // refusal, and `@tr` reaches `.slint` and nothing else — so "More than the
    // 12.5 you can spend now." was permanently English however many catalogues
    // the project grew. The core names the reason now and the words are in
    // `components/note.slint`, which is inside `@tr`'s reach.
    //
    // Driven through the real path: a fixture puts the code and its figure on
    // `SendState`, the screen renders it, and this reads the result back out of
    // the element tree.
    let send = ui.global::<SendState>();
    send.set_amount_note(Note {
        code: "amount-above-spendable".into(),
        args: slint::ModelRc::new(slint::VecModel::from(vec![slint::SharedString::from(
            "12.5000 0000",
        )])),
    });
    ui.set_screen("send".into());

    let sentences = labels(&ui);
    assert!(
        sentences
            .iter()
            .chain(texts(&ui).iter())
            .any(|t| t.contains("Mehr als die 12.5000 0000")),
        "the note is still English: {:?}",
        texts(&ui)
    );

    // And back, because a wallet that can only be switched once is a wallet
    // whose language setting works until somebody changes their mind.
    slint::select_bundled_translation("en").expect("the default language");
    assert!(
        labels(&ui).iter().any(|l| l == "Dashboard"),
        "expected English again after switching back"
    );
}
