//! Rendering the interface into a pixel buffer, with no window.
//!
//! # Why this exists in the library rather than in a test
//!
//! Two callers need it: `examples/render_shots.rs`, which writes PNGs a person
//! can look at, and `tests/visual.rs`, which compares them against checked-in
//! references. Duplicating the setup would let the two drift, and a snapshot
//! test that renders differently from the images you reviewed is worse than no
//! snapshot test.
//!
//! No PNG encoding happens here — this hands back raw RGB and lets the caller
//! decide. That keeps the `image` crate a dev-dependency, so the shipped wallet
//! carries no image codec.
//!
//! # Why offscreen, and not a screenshot of the running window
//!
//! An earlier version pinned the real window to a known rectangle and used
//! macOS `screencapture -R`. It captured an unrelated application the second
//! time it ran, because positioning a window does not raise it and a region
//! capture photographs whatever pixels are there — which on a developer's
//! machine is private. Rendering into a buffer this process owns removes that
//! failure mode rather than narrowing it: there is no screen involved, so there
//! is nothing else that could be in the image.

use std::rc::Rc;

use slint::platform::software_renderer::{
    MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType,
};
use slint::platform::{Platform, WindowAdapter};
use slint::{ComponentHandle, PhysicalSize};

use crate::AppWindow;

/// The size every snapshot is rendered at.
///
/// Fixed so a reference image stays comparable. Wide enough that the nav rail
/// is expanded — the collapsed state is a separate case worth its own snapshot
/// once it matters.
pub const WIDTH: u32 = 1240;
pub const HEIGHT: u32 = 800;

/// One rendered frame: `WIDTH * HEIGHT * 3` bytes, RGB, row-major.
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
}

/// A platform with a single software-rendered window and no event loop.
struct Offscreen {
    window: Rc<MinimalSoftwareWindow>,
}

impl Platform for Offscreen {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }
}

/// Install the offscreen platform. **Callable once per process.**
///
/// Slint allows exactly one platform per process, which is why the visual test
/// renders every case inside a single `#[test]` rather than one test each — and
/// why this returns the window instead of hiding it in a global.
pub fn install() -> Result<Rc<MinimalSoftwareWindow>, Box<dyn std::error::Error>> {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    // `SetPlatformError` is its own type and does not convert into
    // `PlatformError`, so this is mapped rather than propagated with `?`.
    slint::platform::set_platform(Box::new(Offscreen {
        window: window.clone(),
    }))
    .map_err(|e| format!("a Slint platform is already installed: {e:?}"))?;
    Ok(window)
}

/// Render one screen in one theme.
pub fn render(
    window: &Rc<MinimalSoftwareWindow>,
    screen: &str,
    dark: bool,
    seed: impl FnOnce(&AppWindow),
) -> Result<Frame, Box<dyn std::error::Error>> {
    let ui = AppWindow::new()?;

    // A reference image is in English, whatever machine renders it.
    //
    // Slint selects a bundled translation from the **system locale** when the
    // first component is built, and there is a `de` catalogue in the tree. On a
    // German machine — this one — every reference frame would otherwise come
    // out with a German navigation and an English everything else, and would
    // then differ from the same frame rendered anywhere else. An image that
    // depends on who rendered it is not a reference.
    //
    // After `AppWindow::new()`, because the bundle attaches to the context the
    // first component creates. Ignored rather than propagated: a build with no
    // catalogues at all is already English and has nothing to select.
    let _ = slint::select_bundled_translation("en");

    // Freeze the wordmark's cursor, lit.
    //
    // It blinks on a 550ms timer, so a frame captured while it is off differs
    // from the same frame captured 550ms later — and this renderer draws 96
    // screens in a row. That is a visual test that fails at random, which is
    // worse than not having one. The reference images get a cursor that is
    // always there.
    ui.global::<crate::Motion>().set_cursor_blink(false);

    // The chart's callbacks compute its geometry from the element's own size,
    // so they have to be wired before the window is laid out — and installing
    // resets the chart, which is what keeps one case's readings from turning up
    // in the next one's picture.
    crate::chart::install(&ui);
    seed(&ui);
    ui.set_screen(screen.into());
    ui.global::<crate::Theme>().set_dark(dark);

    window.set_size(PhysicalSize::new(WIDTH, HEIGHT));
    ui.show()?;

    let width = WIDTH as usize;
    let mut buffer = vec![
        PremultipliedRgbaColor {
            red: 0,
            green: 0,
            blue: 0,
            alpha: 0,
        };
        width * HEIGHT as usize
    ];

    window.request_redraw();
    let drew = window.draw_if_needed(|renderer| {
        renderer.render(&mut buffer, width);
    });
    ui.hide()?;

    if !drew {
        return Err("nothing was drawn — the window reported no pending repaint".into());
    }

    // The window background is opaque, so alpha is 255 throughout and the
    // premultiplied channels already hold the straight colour values.
    let mut rgb = Vec::with_capacity(buffer.len() * 3);
    for pixel in &buffer {
        rgb.extend_from_slice(&[pixel.red, pixel.green, pixel.blue]);
    }

    Ok(Frame {
        width: WIDTH,
        height: HEIGHT,
        rgb,
    })
}

/// One reference image to render: screen id, file label, and the fixture that
/// puts the window into the state being photographed.
pub type Case = (&'static str, &'static str, fn(&AppWindow));

/// Every screen/state combination that gets a reference image.
///
/// Both themes for each, because the light palette is its own set of values
/// rather than an inversion of the dark one and is otherwise never looked at.
///
/// Note how few of these are actually different *screens*. Onboarding and the
/// backup flow share a screen id and differ only in the starting state, which
/// is the point: what the window shows is decided by the wallet, not by a
/// router the UI drives.
pub const CASES: &[Case] = &[
    ("dashboard", "onboarding", crate::fixtures::fresh),
    ("dashboard", "restore", crate::fixtures::restoring),
    ("dashboard", "dashboard", crate::fixtures::unlocked),
    ("dashboard", "dashboard-funded", crate::fixtures::funded),
    (
        "dashboard",
        "chart-young-wallet",
        crate::fixtures::young_wallet,
    ),
    ("dashboard", "backup-due", crate::fixtures::backup_due),
    ("dashboard", "backup-phrase", crate::fixtures::backup_phrase),
    ("dashboard", "backup-verify", crate::fixtures::backup_verify),
    ("dashboard", "locked", crate::fixtures::locked),
    (
        "dashboard",
        "locked-refused",
        crate::fixtures::locked_refused,
    ),
    ("dashboard", "unconfirmed", crate::fixtures::unconfirmed),
    ("dashboard", "toasts", crate::fixtures::complaining),
    ("activity", "activity", crate::fixtures::funded),
    ("activity", "tx-detail", crate::fixtures::tx_detail),
    ("send", "send-form", crate::fixtures::sending),
    ("send", "send-too-much", crate::fixtures::sending_too_much),
    ("send", "send-review", crate::fixtures::reviewing),
    (
        "send",
        "send-review-identity",
        crate::fixtures::reviewing_identity,
    ),
    ("receive", "receive", crate::fixtures::receiving),
    ("settings", "settings", crate::fixtures::settings),
    ("settings", "keys", crate::fixtures::keys),
    ("settings", "addresses", crate::fixtures::addresses),
    ("settings", "general", crate::fixtures::general_settings),
    ("settings", "keys-renaming", crate::fixtures::renaming_key),
    ("nodes", "network", crate::fixtures::network),
    ("nodes", "network-trouble", crate::fixtures::network_trouble),
    ("identities", "identities", crate::fixtures::identities),
    (
        "identities",
        "claiming-a-name",
        crate::fixtures::claiming_a_name,
    ),
    (
        "identities",
        "claiming-authority",
        crate::fixtures::claiming_authority,
    ),
    (
        "identities",
        "claiming-review",
        crate::fixtures::claiming_review,
    ),
    ("identities", "registering", crate::fixtures::registering),
    (
        "identities",
        "identity-change-review",
        crate::fixtures::identity_change_review,
    ),
    (
        "identities",
        "identity-revoke-review",
        crate::fixtures::identity_revoke_review,
    ),
    (
        "identities",
        "identity-detail",
        crate::fixtures::identity_detail,
    ),
    (
        "identities",
        "identity-authorities",
        crate::fixtures::identity_authorities,
    ),
    ("currencies", "currencies", crate::fixtures::currencies),
    (
        "currencies",
        "currency-kind",
        crate::fixtures::currency_kind,
    ),
    (
        "currencies",
        "currency-identity",
        crate::fixtures::currency_identity,
    ),
    (
        "currencies",
        "currency-form",
        crate::fixtures::defining_currency,
    ),
    ("currencies", "currency-nft", crate::fixtures::currency_nft),
    (
        "currencies",
        "currency-reserve-picker",
        crate::fixtures::currency_reserve_picker,
    ),
    (
        "currencies",
        "currency-review",
        crate::fixtures::currency_review,
    ),
    (
        "currencies",
        "currency-authority",
        crate::fixtures::currency_authority,
    ),
    (
        "currencies",
        "currency-weights-wrong",
        crate::fixtures::currency_weights_wrong,
    ),
    (
        "currencies",
        "launch-review",
        crate::fixtures::launching_currency,
    ),
    (
        "currencies",
        "currency-new-name",
        crate::fixtures::claiming_a_name_for_a_currency,
    ),
    (
        "currencies",
        "launch-pending",
        crate::fixtures::launch_pending,
    ),
    (
        "currencies",
        "currencies-empty",
        crate::fixtures::currencies_empty,
    ),
];
