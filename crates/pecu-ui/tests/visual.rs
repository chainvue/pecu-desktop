//! The interface must not change by accident.
//!
//! Renders every screen offscreen and compares it, pixel for pixel, against a
//! checked-in reference. A layout change is then either a deliberate act with
//! new reference images in the same commit, or a red test — rather than
//! something discovered three weeks later in a screenshot.
//!
//! # Updating the references
//!
//! ```sh
//! UPDATE_SNAPSHOTS=1 cargo test -p pecu-ui --test visual
//! cargo run -p pecu-ui --example render_shots   # then LOOK at docs/shots/
//! ```
//!
//! Updating without looking defeats the point: the test cannot tell an
//! improvement from a regression, only that something moved.
//!
//! It rewrites **every** image, not the ones that failed — including the 116
//! that a machine other than the one which recorded them redraws a rounding
//! step differently, which the comparison forgives and a byte comparison does
//! not. A two-screen change then arrives as a hundred-file diff. Keep what you
//! meant to change and put the rest back:
//!
//! ```sh
//! git status --porcelain -- crates/pecu-ui/tests/snapshots \
//!   | awk '{print $2}' | grep -v <what-you-changed> | xargs -r git checkout --
//! ```
//!
//! # One set for every platform
//!
//! There is a single set of references and every platform compares against it.
//! macOS and Linux do not draw this interface identically — 116 of the 132
//! images differ — but the entire disagreement is 872 pixels off by 1 of 255,
//! sitting in the navigation rail's icon column, where nothing has moved. It
//! is a rounding difference in compositing. See `TOLERANCE` for the numbers
//! and for why allowing it does not blunt this test.
//!
//! This was two sets, one per operating system, for as long as it took to find
//! out what the difference actually was. Two sets are worse than a tolerance:
//! a change reviewed on one platform lands red on the others, and the only
//! response available is a blanket re-record — which verifies nothing and
//! teaches the habit that ends snapshot testing. Windows would have made it
//! three sets and 396 images.
//!
//! Re-recording on any platform is fine. A set recorded on Linux differs from
//! one recorded on macOS by the same 1 of 255, which is inside the tolerance
//! in either direction, so the references cannot drift by being touched from
//! the wrong desk.
//!
//! # Why everything happens in one `#[test]`
//!
//! Slint permits exactly one platform per process, and `snapshot::install()`
//! sets it. Two test functions would race to install it and the second would
//! fail. One test, every case in sequence — and every failure collected before
//! reporting, so a change that moves four screens is one message rather than
//! four runs.

use std::path::{Path, PathBuf};

use pecu_ui::snapshot;

/// Per-channel tolerance for a pixel to count as changed.
///
/// One. This said zero, and asked that a reason be written down before it was
/// ever raised. Here is the reason; the old note was right about the danger
/// and wrong about the number.
///
/// macOS and Linux disagree about this interface on 116 of the 132 images, on
/// 872 pixels in total, and **every one of those pixels is off by exactly 1**.
/// Not one to ten — one. They sit in the navigation rail's icon column and
/// nothing about the layout differs, so it is a rounding difference in how a
/// colour is composited, not geometry and not a subpixel edge. At zero that is
/// 116 red images that mean nothing, and the answer to a test that is red for
/// no reason is a blanket re-record.
///
/// A tolerance of 1 does not hide what this test exists to catch. Measured
/// against 3cfbd84, which moved the network screens into a Settings tab:
/// 2,096,525 pixels changed, 99.65% of them by more than 1, and not one of the
/// 120 affected images would have had its change absorbed. A real move crosses
/// contrast edges; rounding stays inside them.
///
/// What a tolerance of 1 could in principle swallow is a hairline in a colour
/// close to its background, shifted by one pixel. That is what `NOISE_BUDGET`
/// is for.
const TOLERANCE: u8 = 1;

/// How many rounding-noise pixels one image may carry before it fails anyway.
///
/// Fifty, against a measured worst case of ten — five times the headroom, and
/// still far below the hundreds of pixels a shifted hairline would move. The
/// tolerance decides how *strong* a difference may be and this decides how
/// *much* of it there may be, which is the half that catches a real change
/// made of weak differences.
///
/// It is also the tripwire under the measurement above. A Slint version or a
/// third platform that pushes the rounding noise up says so here, instead of
/// quietly spending a tolerance that was granted on the strength of ten.
const NOISE_BUDGET: u64 = 50;

fn snapshot_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots")
}

#[test]
fn the_interface_matches_its_reference_images() {
    let updating = std::env::var("UPDATE_SNAPSHOTS").as_deref() == Ok("1");
    let dir = snapshot_dir();
    std::fs::create_dir_all(&dir).expect("snapshot directory");

    let window = snapshot::install().expect("offscreen platform");
    let mut failures = Vec::new();
    let mut written = Vec::new();

    for (screen, label, seed) in snapshot::CASES {
        for dark in [true, false] {
            let theme = if dark { "dark" } else { "light" };
            let name = format!("{label}-{theme}");
            let path = dir.join(format!("{name}.png"));

            let frame = snapshot::render(&window, screen, dark, *seed)
                .unwrap_or_else(|e| panic!("rendering {name} failed: {e}"));

            let actual = image::RgbImage::from_raw(frame.width, frame.height, frame.rgb)
                .expect("frame dimensions match the buffer");

            if updating || !path.exists() {
                actual.save(&path).expect("write reference image");
                written.push(name);
                continue;
            }

            let expected = image::open(&path)
                .unwrap_or_else(|e| panic!("reading reference {}: {e}", path.display()))
                .to_rgb8();

            if let Some(problem) = compare(&expected, &actual, &dir, &name) {
                failures.push(format!("{name}: {problem}"));
            }
        }
    }

    if !written.is_empty() {
        // Not a silent pass: writing a reference means nothing was verified, and
        // saying so is the difference between "approved" and "recorded".
        eprintln!(
            "wrote {} reference image(s) — these were RECORDED, not verified: {}",
            written.len(),
            written.join(", "),
        );
    }

    assert!(
        failures.is_empty(),
        "the interface changed:\n  {}\n\n\
         A diff image was written beside each reference. If the change was \
         intended, re-record with:\n    \
         UPDATE_SNAPSHOTS=1 cargo test -p pecu-ui --test visual\n  \
         and look at docs/shots/ before committing.",
        failures.join("\n  "),
    );
}

/// Compare two frames, writing a diff image when they differ.
///
/// Returns `None` when they match. The diff marks changed pixels in magenta
/// and rounding noise in amber, over a dimmed copy of the expected image, so
/// *where* the change is — and which of the two it is — can be seen at a
/// glance rather than inferred from a percentage.
fn compare(
    expected: &image::RgbImage,
    actual: &image::RgbImage,
    dir: &Path,
    name: &str,
) -> Option<String> {
    if expected.dimensions() != actual.dimensions() {
        return Some(format!(
            "size changed: reference is {:?}, render is {:?}",
            expected.dimensions(),
            actual.dimensions()
        ));
    }

    let mut diff = image::RgbImage::new(expected.width(), expected.height());
    let mut changed = 0u64;
    let mut noise = 0u64;

    for (x, y, reference) in expected.enumerate_pixels() {
        let rendered = actual.get_pixel(x, y);
        let delta = reference
            .0
            .iter()
            .zip(rendered.0.iter())
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap_or(0);

        if delta > TOLERANCE {
            changed += 1;
            diff.put_pixel(x, y, image::Rgb([255, 0, 200]));
        } else if delta > 0 {
            // Amber, not magenta: this is the rounding the tolerance forgives,
            // and it should not look like the thing that failed.
            noise += 1;
            diff.put_pixel(x, y, image::Rgb([255, 176, 0]));
        } else {
            // Dimmed, so the magenta reads as an overlay on recognisable
            // furniture rather than floating in the dark.
            diff.put_pixel(
                x,
                y,
                image::Rgb([reference.0[0] / 3, reference.0[1] / 3, reference.0[2] / 3]),
            );
        }
    }

    if changed == 0 && noise <= NOISE_BUDGET {
        return None;
    }

    let diff_path = dir.join(format!("{name}.diff.png"));
    let _ = diff.save(&diff_path);

    let total = u64::from(expected.width()) * u64::from(expected.height());
    // Basis points, in integer arithmetic. The workspace denies lossy casts on
    // money paths and it is not worth carving an exception for a progress
    // figure — an image is at most a few million pixels, so this is exact.
    let bps = changed.saturating_mul(10_000) / total.max(1);
    let percent = format!("{}.{:02}", bps / 100, bps % 100);

    // Which rule broke, rather than one number that could mean either. A run
    // over the budget with nothing else changed is the interesting case: the
    // layout is where it was, and something about how it is drawn is not.
    let problem = if changed > 0 {
        format!("{changed} pixels differ ({percent}%)")
    } else {
        format!(
            "{noise} pixels differ by one, over a budget of {NOISE_BUDGET} — \
             nothing moved, but something about how this is drawn changed"
        )
    };
    Some(format!("{problem} — see {}", diff_path.display()))
}
