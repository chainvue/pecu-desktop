//! A QR code for the receive address.
//!
//! # Five rules, and skipping any one is what makes a QR widget look furry
//!
//! 1. Allocate the buffer in **physical** pixels — logical × scale factor. On a
//!    Retina display a logically-sized buffer is half the resolution it needs.
//! 2. The module size must be a **whole number** of physical pixels, so every
//!    module is a perfect square of identical pixels. A fractional size means
//!    some modules come out a pixel wider than others, which is exactly the
//!    artefact that makes a scanner hesitate.
//! 3. Display at `side / scale_factor` logical pixels, so one buffer pixel is
//!    one device pixel.
//! 4. `image-rendering: pixelated` at the call site, so any residual scaling is
//!    nearest-neighbour rather than a blur.
//! 5. `image-fit: contain`, never inside a layout that stretches it.
//!
//! Rules 1–3 are here; 4 and 5 are in `app.slint`, where the element lives.
//!
//! # The quiet zone is inside the image
//!
//! Four modules of white on every side, baked into the buffer rather than
//! achieved with padding. A QR code without its quiet zone is unreliable to
//! scan, and putting it in the buffer means the image is correct on its own —
//! including if it is ever saved out.
//!
//! # Always on white
//!
//! Even in the dark theme. Inverted QR codes fail on a fair number of scanners,
//! which is a strange thing to discover while trying to be paid.

use slint::{Image, Rgb8Pixel, SharedPixelBuffer};

/// Modules of quiet zone on each side. Four is the specification's minimum.
const QUIET: usize = 4;

/// A QR code for `payload`, sized to fit a `side_logical` box.
///
/// `None` when there is nothing to encode or the payload is too long for any QR
/// version — a blank space beats a broken image, and the address is on screen
/// as text beside it either way.
pub fn encode(payload: &str, side_logical: f32, scale_factor: f32) -> Option<Image> {
    if payload.is_empty() {
        return None;
    }

    // `EcLevel::M` — 15 % recovery. Enough for a screen, where the failure mode
    // is a bad camera angle rather than a coffee stain, and it keeps the matrix
    // small enough that every module gets several pixels.
    let code = qrcode::QrCode::with_error_correction_level(payload, qrcode::EcLevel::M).ok()?;

    let modules = code.width() + QUIET * 2;

    // Rules 1 and 2: the largest WHOLE number of physical pixels per module
    // that still fits the box.
    //
    // Integer division, not a float floor. A QR matrix is at most 177 modules
    // and a window is at most a few thousand pixels, so `u32` is roomy — and it
    // sidesteps the question of whether a `f64` floor lands on the value below
    // the one you meant.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let target = (side_logical.max(1.0) * scale_factor.max(1.0)) as u32;
    let module_px = usize::try_from((target / u32::try_from(modules).ok()?).max(1)).ok()?;

    let (side, dark) = paint(&code, module_px)?;
    let width = u32::try_from(side).ok()?;

    let mut buffer = SharedPixelBuffer::<Rgb8Pixel>::new(width, width);
    let pixels = buffer.make_mut_slice();

    for (pixel, dark) in pixels.iter_mut().zip(dark) {
        // Near-black rather than pure black: it matches the palette, and
        // scanners threshold rather than measure.
        *pixel = if dark {
            Rgb8Pixel::new(11, 12, 14)
        } else {
            Rgb8Pixel::new(255, 255, 255)
        };
    }

    Some(Image::from_rgb8(buffer))
}

/// The pixel grid, one `bool` per pixel, row-major, `true` for dark.
///
/// Split out from [`encode`] so it can be checked against the library's own
/// matrix. A transposed loop or an off-by-one in the quiet zone produces a
/// picture that looks exactly like a QR code and encodes something else — which
/// is not a defect anyone finds by looking at a screenshot.
///
/// Returns the side length in pixels alongside the grid.
fn paint(code: &qrcode::QrCode, module_px: usize) -> Option<(usize, Vec<bool>)> {
    let width = code.width();
    let modules = width + QUIET * 2;
    let side = modules.checked_mul(module_px)?;

    let colors = code.to_colors();
    let mut dark = vec![false; side.checked_mul(side)?];

    for (index, color) in colors.iter().enumerate() {
        if *color != qrcode::Color::Dark {
            continue;
        }
        // `to_colors` is row-major: index = row * width + column.
        let x = (index % width + QUIET) * module_px;
        let y = (index / width + QUIET) * module_px;

        for row in y..y + module_px {
            let start = row * side + x;
            dark[start..start + module_px].fill(true);
        }
    }

    Some((side, dark))
}

/// The logical size to display an image at so one buffer pixel lands on exactly
/// one device pixel — rule 3.
pub fn logical_side(image: &Image, scale_factor: f32) -> f32 {
    #[allow(clippy::cast_precision_loss)]
    let physical = image.size().width as f32;
    physical / scale_factor.max(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADDRESS: &str = "RQr2cUkF46n7y8WRzDkd1iV9gHusSSQuzX";

    fn module_count() -> u32 {
        let code = qrcode::QrCode::with_error_correction_level(ADDRESS, qrcode::EcLevel::M)
            .expect("the fixture address encodes");
        u32::try_from(code.width() + QUIET * 2).expect("a QR matrix is small")
    }

    #[test]
    fn nothing_to_encode_yields_nothing() {
        assert!(encode("", 240.0, 2.0).is_none());
    }

    /// Rule 2, asserted: the buffer is a whole number of modules across at
    /// every scale factor and box size, so no module is a pixel wider than its
    /// neighbour.
    #[test]
    fn the_image_is_a_whole_number_of_modules() {
        let modules = module_count();

        for scale in [1.0f32, 2.0, 3.0] {
            for side in [180.0f32, 240.0, 260.0] {
                let image = encode(ADDRESS, side, scale).expect("an address encodes");
                let physical = image.size().width;

                assert_eq!(image.size().height, physical, "not square");
                assert_eq!(
                    physical % modules,
                    0,
                    "{physical} physical pixels do not divide into {modules} modules \
                     at scale {scale}, box {side}",
                );
                assert!(physical / modules >= 1, "the modules collapsed to nothing");

                // And it must not overflow the box it was asked to fit.
                #[allow(clippy::cast_precision_loss)]
                let logical = physical as f32 / scale;
                assert!(logical <= side, "{logical} logical exceeds the {side} box");
            }
        }
    }

    /// A Retina display has to get twice the pixels, or rule 1 is decoration.
    #[test]
    fn a_higher_scale_factor_yields_a_larger_buffer() {
        let one = encode(ADDRESS, 240.0, 1.0).expect("encodes");
        let two = encode(ADDRESS, 240.0, 2.0).expect("encodes");
        assert!(
            two.size().width > one.size().width,
            "a 2× display got the same buffer as a 1× one: {} vs {}",
            two.size().width,
            one.size().width,
        );

        // Rule 3: both display at the same logical size, within one module.
        let a = logical_side(&one, 1.0);
        let b = logical_side(&two, 2.0);
        assert!((a - b).abs() < 240.0 / 8.0, "{a} and {b} differ too much");
    }

    /// The pixels have to say what the library says they should.
    ///
    /// This is the test that catches a transposed loop: an x/y swap renders a
    /// perfectly plausible QR code that decodes to something else, and no
    /// amount of looking at a screenshot would find it.
    #[test]
    fn every_pixel_matches_the_librarys_own_matrix() {
        let code = qrcode::QrCode::with_error_correction_level(ADDRESS, qrcode::EcLevel::M)
            .expect("the fixture address encodes");
        let colors = code.to_colors();
        let width = code.width();

        for module_px in [1usize, 3, 7] {
            let (side, dark) = paint(&code, module_px).expect("a grid");
            assert_eq!(side, (width + QUIET * 2) * module_px);

            for (index, color) in colors.iter().enumerate() {
                let expected = *color == qrcode::Color::Dark;
                let mx = index % width;
                let my = index / width;

                // Every pixel of the module, not just its corner — that is what
                // proves the block fill covers exactly one module.
                for dy in 0..module_px {
                    for dx in 0..module_px {
                        let x = (mx + QUIET) * module_px + dx;
                        let y = (my + QUIET) * module_px + dy;
                        assert_eq!(
                            dark[y * side + x],
                            expected,
                            "module ({mx},{my}) pixel ({dx},{dy}) at {module_px}px",
                        );
                    }
                }
            }

            // And the quiet zone is untouched on all four sides.
            let quiet = QUIET * module_px;
            for y in 0..side {
                for x in 0..side {
                    let inside = x >= quiet && y >= quiet && x < side - quiet && y < side - quiet;
                    if !inside {
                        assert!(!dark[y * side + x], "the quiet zone has ink at ({x},{y})");
                    }
                }
            }
        }
    }

    /// A box too small for the code still yields one physical pixel per module
    /// rather than an empty buffer.
    #[test]
    fn an_impossibly_small_box_still_yields_one_pixel_per_module() {
        let image = encode(ADDRESS, 4.0, 1.0).expect("still encodes");
        assert_eq!(image.size().width, module_count());
    }
}
