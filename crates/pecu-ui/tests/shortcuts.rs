//! The keyboard shortcuts actually fire.
//!
//! # Why this exists
//!
//! Because they did not. They were written, reviewed, reasoned about at length
//! — the scope encloses the interface, Slint bubbles a key from the focused
//! item up towards the window, therefore ⌘L reaches it — and shipped, and every
//! word of that reasoning was right except that nothing ever pressed a key.
//!
//! Slint tracks modifiers through key events of their own: pressing Command
//! arrives as `Key::Meta` and updates the tracked state, and the next key event
//! carries it. So a test can hold a modifier down, press a letter, and watch
//! what happens — which is the only way to know.

#![allow(clippy::expect_used, clippy::panic)]

use std::cell::Cell;
use std::rc::Rc;

use pecu_ui::{snapshot, Actions, AppWindow, MarketState, WalletState};
use slint::platform::{Key, WindowEvent};
use slint::{ComponentHandle, SharedString};

/// Press a key, with whatever modifiers are already held.
fn press(window: &AppWindow, text: impl Into<SharedString>) {
    window
        .window()
        .dispatch_event(WindowEvent::KeyPressed { text: text.into() });
}

fn release(window: &AppWindow, text: impl Into<SharedString>) {
    window
        .window()
        .dispatch_event(WindowEvent::KeyReleased { text: text.into() });
}

/// Hold `modifier`, press `key`, let go.
fn chord(window: &AppWindow, modifier: Key, key: &str) {
    let modifier: SharedString = modifier.into();
    press(window, modifier.clone());
    press(window, key);
    release(window, key);
    release(window, modifier);
}

/// An unlocked wallet, so the shell is on screen rather than the unlock form.
fn unlocked_window(
    window: &Rc<slint::platform::software_renderer::MinimalSoftwareWindow>,
) -> AppWindow {
    let ui = AppWindow::new().expect("a window");
    pecu_ui::chart::install(&ui);

    let wallet = ui.global::<WalletState>();
    wallet.set_loading(false);
    wallet.set_exists(true);
    wallet.set_locked(false);

    window.set_size(slint::PhysicalSize::new(snapshot::WIDTH, snapshot::HEIGHT));
    ui.show().expect("show");
    // A layout pass, so focus and geometry are real before any key arrives.
    window.request_redraw();
    let _ = window.draw_if_needed(|_| {});
    ui
}

#[test]
fn the_keyboard_reaches_the_wallet() {
    let window = snapshot::install().expect("offscreen platform");
    let ui = unlocked_window(&window);

    // ── Navigation ──────────────────────────────────────────────────────
    let navigated: Rc<Cell<bool>> = Rc::default();
    {
        let navigated = navigated.clone();
        ui.global::<Actions>()
            .on_navigate(move |_| navigated.set(true));
    }

    // The numbers follow the rail. It gained Markets and then Convert at the
    // top, so these moved with it — deliberately, rather than letting the rail
    // and the shortcuts drift into two arrangements to learn.
    chord(&ui, Key::Meta, "5");
    assert_eq!(
        ui.get_screen(),
        "receive",
        "Command-5 did not reach the window",
    );
    assert!(
        navigated.get(),
        "the core was never told the screen changed"
    );

    // Control as well as Command, because the same build runs on Linux and
    // Windows and a shortcut that only works on a Mac is not a shortcut.
    chord(&ui, Key::Control, "6");
    assert_eq!(ui.get_screen(), "activity", "Control-6 did not reach it");

    // ── Lock ────────────────────────────────────────────────────────────
    let locked: Rc<Cell<bool>> = Rc::default();
    {
        let locked = locked.clone();
        ui.global::<Actions>().on_lock(move || locked.set(true));
    }
    chord(&ui, Key::Meta, "l");
    assert!(locked.get(), "Command-L did not lock the wallet");

    // ── After Tab has moved focus into a control ────────────────────────
    //
    // This is the state somebody is actually in. Focus sits on a button's own
    // scope, which rejects everything but Return and Space — so the key has to
    // travel up through it to reach the shortcuts. If that chain is broken the
    // shortcuts work exactly until the first Tab, which is to say never.
    locked.set(false);
    press(&ui, SharedString::from(Key::Tab));
    release(&ui, SharedString::from(Key::Tab));
    chord(&ui, Key::Meta, "l");
    assert!(
        locked.get(),
        "Command-L stopped working once Tab had moved focus into a control",
    );

    // ── And with focus in a text field ──────────────────────────────────
    //
    // The other place focus actually sits. A `TextInput` consumes characters,
    // and if it consumed modified ones too the shortcuts would die the moment
    // somebody clicked into the send form.
    locked.set(false);
    for _ in 0..12 {
        press(&ui, SharedString::from(Key::Tab));
        release(&ui, SharedString::from(Key::Tab));
    }
    chord(&ui, Key::Meta, "l");
    assert!(
        locked.get(),
        "Command-L stopped working with focus somewhere deeper in the screen",
    );

    // ── And an unmodified letter is still just a letter ──────────────────
    //
    // The shortcut table is guarded on a modifier, so this could only fail if
    // one were added without one. Asserted rather than assumed: the failure is
    // somebody's passphrase losing a character, which nothing else would catch.
    locked.set(false);
    press(&ui, "l");
    release(&ui, "l");
    assert!(!locked.get(), "an unmodified `l` locked the wallet");
}

/// Escape leaves an open market, and does not fire when none is open.
///
/// The market detail is a **view**, not an overlay: there is no scrim to click
/// away and the nav rail stays live behind it. So Escape and the breadcrumb are
/// the whole way back, and if Escape stops reaching it the only route out is a
/// link somebody has to find with a pointer.
///
/// The second half matters as much as the first. Escape falls through a chain
/// of dismissals in `app.slint` and ends at `EventResult.reject`; a branch that
/// fired when nothing was open would swallow the key from whatever is added
/// below it later.
#[test]
fn escape_closes_an_open_market() {
    let window = snapshot::install().expect("offscreen platform");
    let ui = unlocked_window(&window);

    let asked: Rc<Cell<i32>> = Rc::default();
    let market = ui.global::<MarketState>();
    {
        let asked = asked.clone();
        let ui = ui.as_weak();
        market.on_select(move |address| {
            asked.set(asked.get() + 1);
            // What `wire_markets` does in the real application: write it
            // locally, then tell the core. Without the local write the property
            // never changes and this test would pass on a callback that did
            // nothing.
            if let Some(ui) = ui.upgrade() {
                ui.global::<MarketState>().set_selected(address);
            }
        });
    }

    ui.set_screen("markets".into());
    market.set_selected("iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq".into());

    press(&ui, SharedString::from(Key::Escape));
    release(&ui, SharedString::from(Key::Escape));

    assert_eq!(
        ui.global::<MarketState>().get_selected(),
        "",
        "Escape did not leave the market detail",
    );
    assert_eq!(asked.get(), 1, "the core was never told the market closed");

    // Nothing open: Escape must not reach for the callback again.
    press(&ui, SharedString::from(Key::Escape));
    release(&ui, SharedString::from(Key::Escape));
    assert_eq!(
        asked.get(),
        1,
        "Escape fired at a screen with no market open, so it would swallow the \
         key from anything added to the chain below it",
    );
}
