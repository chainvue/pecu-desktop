//! Render the application icon, once per size macOS asks for.
//!
//! ```sh
//! cargo run -p chainvue-ui --example render_icon
//! ```
//!
//! Writes `crates/chainvue-app/assets/ChainVue.iconset/`, which
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
//! And it is rendered in the **light** palette explicitly rather than in both:
//! an application icon does not follow the system theme, and one that was
//! rendered in whichever mode happened to be set would change colour depending
//! on who packaged the build.

use std::rc::Rc;

use slint::platform::software_renderer::{
    MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType,
};
use slint::platform::{Platform, WindowAdapter};
use slint::{ComponentHandle, PhysicalSize};

use chainvue_ui::{AppIcon, Theme};

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

    let assets = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../chainvue-app/assets");
    let iconset = assets.join("ChainVue.iconset");
    std::fs::create_dir_all(&iconset)?;

    for (name, side) in SIZES {
        let image = draw(&window, *side)?;
        image.save(iconset.join(name))?;
        println!("{}", iconset.join(name).display());
    }

    // The same 1024 render on its own, for anything that wants one file — a
    // README, a web page, a Linux `.desktop` entry when there is one.
    let largest = draw(&window, 1024)?;
    let path = assets.join("icon-1024.png");
    largest.save(&path)?;
    println!("{}", path.display());

    Ok(())
}

/// One size, drawn at exactly that size.
fn draw(
    window: &Rc<MinimalSoftwareWindow>,
    side: u32,
) -> Result<image::RgbaImage, Box<dyn std::error::Error>> {
    let icon = AppIcon::new()?;
    // The light accent, deliberately and always — see the note at the top.
    icon.global::<Theme>().set_dark(false);
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
