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
//! # Why everything happens in one `#[test]`
//!
//! Slint permits exactly one platform per process, and `snapshot::install()`
//! sets it. Two test functions would race to install it and the second would
//! fail. One test, every case in sequence — and every failure collected before
//! reporting, so a change that moves four screens is one message rather than
//! four runs.

use std::path::{Path, PathBuf};

use pecu_ui::snapshot;

/// Per-channel tolerance.
///
/// Zero. The software renderer is deterministic on a given Slint version, and a
/// tolerance is where a real one-pixel misalignment goes to hide. If this ever
/// has to be raised, the reason belongs here in writing.
const TOLERANCE: u8 = 0;

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
/// over a dimmed copy of the expected image, so *where* the change is can be
/// seen at a glance rather than inferred from a percentage.
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

    for (x, y, reference) in expected.enumerate_pixels() {
        let rendered = actual.get_pixel(x, y);
        let differs = reference
            .0
            .iter()
            .zip(rendered.0.iter())
            .any(|(a, b)| a.abs_diff(*b) > TOLERANCE);

        if differs {
            changed += 1;
            diff.put_pixel(x, y, image::Rgb([255, 0, 200]));
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

    if changed == 0 {
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
    Some(format!(
        "{changed} pixels differ ({percent}%) — see {}",
        diff_path.display()
    ))
}
