//! Render the application icon, once per size macOS asks for.
//!
//! ```sh
//! cargo run -p pecu-ui --example render_icon
//! ```
//!
//! Writes `crates/pecu-app/assets/Pecu.iconset/`, which
//! `scripts/bundle.sh` hands to `iconutil`. Run it after changing the palette or
//! `ui/icon.slint`; the result is checked in, because a build that had to render
//! its own icon would need this renderer on every machine that packages the
//! wallet.
//!
//! # Why every size is drawn rather than one size shrunk
//!
//! Because shrinking loses the icon at the sizes it matters most. The first
//! version rendered 1024px once and let `sips` downsample: at 16px — the Finder
//! list, the window proxy, the ⌘-Tab strip — the ring and the dot inside it blur
//! into a single blob, and the mark stops being a mark. Drawing at 16px instead
//! gives the renderer whole pixels to put the stroke on.
//!
//! The component is resolution-independent, so this costs nothing but a loop.
//!
//! # Why this is not `render_shots`
//!
//! Two differences, and both matter. The icon keeps its **alpha** — macOS
//! composites it over a dock and a Finder row, and an opaque square would be a
//! square — where a screenshot is opaque by definition and throws alpha away.
//! And it is rendered in one palette explicitly rather than in both: an
//! application icon does not follow the system theme, and one that was rendered
//! in whichever mode happened to be set would change colour depending on who
//! packaged the build.
//!
//! That palette is the **dark** one, which is a change and not an oversight.
//! The Pecu mark is defined on ink in neon mint — `#0B0D10` and `#12D6B4` — and
//! those are the dark theme's two values. The light theme's mint is a darker,
//! corrected colour that exists so text can clear 4.5:1 on a white card; it is
//! the right answer to a question the icon is not asking.

use std::rc::Rc;

use slint::platform::software_renderer::{
    MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType,
};
use slint::platform::{Platform, WindowAdapter};
use slint::{ComponentHandle, PhysicalSize};

use pecu_ui::{AppIcon, Theme};

/// The ten files `iconutil` wants, and the pixel size each one is.
///
/// Two names per pixel size in the middle of the range, deliberately: an `@2x`
/// entry is the same *point* size at twice the pixels, and leaving them out
/// gives a blurry icon on every Retina display — which is every Mac sold in a
/// decade.
const SIZES: &[(&str, u32)] = &[
    ("icon_16x16.png", 16),
    ("icon_16x16@2x.png", 32),
    ("icon_32x32.png", 32),
    ("icon_32x32@2x.png", 64),
    ("icon_128x128.png", 128),
    ("icon_128x128@2x.png", 256),
    ("icon_256x256.png", 256),
    ("icon_256x256@2x.png", 512),
    ("icon_512x512.png", 512),
    ("icon_512x512@2x.png", 1024),
];

struct Offscreen {
    window: Rc<MinimalSoftwareWindow>,
}

impl Platform for Offscreen {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    slint::platform::set_platform(Box::new(Offscreen {
        window: window.clone(),
    }))
    .map_err(|e| format!("a Slint platform is already installed: {e:?}"))?;

    let assets = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../pecu-app/assets");
    let iconset = assets.join("Pecu.iconset");
    std::fs::create_dir_all(&iconset)?;

    for (name, side) in SIZES {
        let image = draw(&window, *side)?;
        image.save(iconset.join(name))?;
        println!("{}", iconset.join(name).display());
    }

    // The same 1024 render on its own, for anything that wants one file — a
    // README, a web page, a store listing.
    let largest = draw(&window, 1024)?;
    let path = assets.join("icon-1024.png");
    largest.save(&path)?;
    println!("{}", path.display());

    // ── Linux ───────────────────────────────────────────────────────────────
    //
    // The hicolor theme, which is where every desktop environment looks. One
    // directory per size, each holding a file named after the `Icon=` key in
    // the `.desktop` entry — the name is the link between the two, not a path.
    //
    // Drawn per size for the same reason the macOS set is: a launcher shows
    // this at 48, a window list at 24, and both of those are sizes a 512px
    // render blurs.
    let hicolor = assets.join("hicolor");
    for side in LINUX_SIZES {
        let dir = hicolor.join(format!("{side}x{side}")).join("apps");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("pecu.png");
        draw(&window, *side)?.save(&path)?;
        println!("{}", path.display());
    }

    // ── Windows ─────────────────────────────────────────────────────────────
    let path = assets.join("pecu.ico");
    std::fs::write(&path, ico(&window)?)?;
    println!("{}", path.display());

    Ok(())
}

/// The sizes the hicolor theme is normally installed at.
///
/// 24 and 48 are here and are in neither of the other two sets: 48 is the
/// launcher size on GNOME and KDE both, and 24 is the window list. Leaving them
/// out means the desktop picks the nearest and scales it, which is the blur
/// this whole file exists to avoid.
const LINUX_SIZES: &[u32] = &[16, 24, 32, 48, 64, 128, 256, 512];

/// The sizes inside the Windows `.ico`.
///
/// 256 is the one Explorer's largest view uses and the one that has to be
/// there; the rest are the shell's fixed sizes. 24 is the small-toolbar size.
const ICO_SIZES: &[u32] = &[16, 24, 32, 48, 64, 128, 256];

/// A Windows icon file, assembled by hand.
///
/// # Why this is written out rather than pulled in
///
/// Because an `.ico` is a header, one sixteen-byte record per image, and the
/// images — and every crate that writes one would come with its own PNG encoder
/// beside the one already here. Forty lines against a dependency tree.
///
/// The images are stored as **PNG**, which the shell has understood since
/// Vista. That is the same choice `iconutil` makes for the `.icns`, and it
/// keeps the alpha exact rather than going through a BMP with an AND mask.
///
/// **Untested on Windows.** Nothing here has been opened by Explorer — this
/// machine is a Mac — so what is verified is that the container parses back to
/// the sizes that went in. See `docs/LATER.md` §7.
fn ico(window: &Rc<MinimalSoftwareWindow>) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut images: Vec<(u32, Vec<u8>)> = Vec::new();
    for side in ICO_SIZES {
        let mut png = std::io::Cursor::new(Vec::new());
        draw(window, *side)?.write_to(&mut png, image::ImageFormat::Png)?;
        images.push((*side, png.into_inner()));
    }

    let count = u16::try_from(images.len())?;
    let mut out = Vec::new();
    // ICONDIR: reserved, type 1 (icon, as opposed to 2 for a cursor), count.
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());

    // Every entry is sixteen bytes and they all precede the first image.
    let mut offset = 6 + 16 * u32::from(count);
    for (side, png) in &images {
        // 256 is written as 0: the field is one byte and 256 does not fit, and
        // zero is the format's way of saying "the largest".
        let dimension = u8::try_from(*side).unwrap_or(0);
        out.push(dimension);
        out.push(dimension);
        out.push(0); // palette size — none, this is truecolour
        out.push(0); // reserved
        out.extend_from_slice(&1u16.to_le_bytes()); // colour planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&u32::try_from(png.len())?.to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += u32::try_from(png.len())?;
    }
    for (_, png) in &images {
        out.extend_from_slice(png);
    }
    Ok(out)
}

/// One size, drawn at exactly that size.
fn draw(
    window: &Rc<MinimalSoftwareWindow>,
    side: u32,
) -> Result<image::RgbaImage, Box<dyn std::error::Error>> {
    let icon = AppIcon::new()?;
    // Ink and neon mint, deliberately and always — see the note at the top.
    icon.global::<Theme>().set_dark(true);
    // `f32` because that is what a Slint length is; every size here is a small
    // power of two and exact.
    #[allow(clippy::cast_precision_loss)]
    icon.set_canvas(side as f32);

    window.set_size(PhysicalSize::new(side, side));
    icon.show()?;

    let width = side as usize;
    let mut buffer = vec![
        PremultipliedRgbaColor {
            red: 0,
            green: 0,
            blue: 0,
            alpha: 0,
        };
        width * width
    ];

    window.request_redraw();
    let drew = window.draw_if_needed(|renderer| {
        renderer.render(&mut buffer, width);
    });
    icon.hide()?;

    if !drew {
        return Err(format!("nothing was drawn at {side}px").into());
    }

    // Un-premultiplied, because PNG stores straight colour. Skipping this
    // darkens every partly transparent pixel, which on a rounded rectangle is
    // the entire edge — the corners would come out with a grey fringe against a
    // light background.
    let mut rgba = Vec::with_capacity(buffer.len() * 4);
    for pixel in &buffer {
        let straight = |channel: u8| -> u8 {
            if pixel.alpha == 0 {
                0
            } else {
                // Rounded rather than truncated: truncation loses a level on
                // every channel and shows up as a seam where two flat colours
                // meet at the same alpha.
                u8::try_from(
                    (u32::from(channel) * 255 + u32::from(pixel.alpha) / 2)
                        / u32::from(pixel.alpha),
                )
                .unwrap_or(255)
            }
        };
        rgba.extend_from_slice(&[
            straight(pixel.red),
            straight(pixel.green),
            straight(pixel.blue),
            pixel.alpha,
        ]);
    }

    image::RgbaImage::from_raw(side, side, rgba)
        .ok_or_else(|| "buffer size did not match the image dimensions".into())
}
