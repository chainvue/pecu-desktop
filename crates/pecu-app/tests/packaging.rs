//! The icon and the desktop entry, checked as files.
//!
//! # Why this is a test and not a look in a folder
//!
//! Because the three platforms take the icon three different ways and only one
//! of them can be tried here. macOS is verifiable — `scripts/bundle.sh` builds
//! the bundle on this machine and the Dock shows the result. Windows and Linux
//! are not: the assets are generated, checked in, and until now nothing said
//! whether they were even the right shape.
//!
//! So this asserts what is checkable anywhere: that the files exist, that the
//! `.ico` container parses back to the sizes that went into it, and that the
//! one string tying the desktop entry to its icons — `Icon=pecu` — matches the
//! filenames beside it. That last one is the whole failure mode of a `.desktop`
//! file: it is a name looked up in a theme, not a path, so a rename that misses
//! one of the two produces a launcher entry with a generic gear on it and no
//! error anywhere.
//!
//! Regenerate the assets with:
//!
//! ```sh
//! cargo run -p pecu-ui --example render_icon
//! ```

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};

fn assets() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("assets")
}

/// The sizes the hicolor theme is installed at. Mirrors `LINUX_SIZES` in
/// `render_icon.rs`; the duplication is the point, since agreeing is what is
/// being checked.
const LINUX_SIZES: &[u32] = &[16, 24, 32, 48, 64, 128, 256, 512];

/// What the desktop entry's `Icon=` key says, and therefore what every file in
/// the icon theme has to be called.
const ICON_NAME: &str = "pecu";

#[test]
fn the_macos_iconset_holds_every_size_iconutil_asks_for() {
    let iconset = assets().join("Pecu.iconset");
    for name in [
        "icon_16x16.png",
        "icon_16x16@2x.png",
        "icon_32x32.png",
        "icon_32x32@2x.png",
        "icon_128x128.png",
        "icon_128x128@2x.png",
        "icon_256x256.png",
        "icon_256x256@2x.png",
        "icon_512x512.png",
        "icon_512x512@2x.png",
    ] {
        let path = iconset.join(name);
        assert!(path.is_file(), "{} is missing", path.display());
        assert!(
            std::fs::metadata(&path).expect("stat").len() > 0,
            "{} is empty",
            path.display(),
        );
    }
}

/// One directory per size, each holding a file named after the `Icon=` key.
#[test]
fn the_hicolor_theme_is_complete_and_named_after_the_desktop_entry() {
    for side in LINUX_SIZES {
        let path = assets()
            .join("hicolor")
            .join(format!("{side}x{side}"))
            .join("apps")
            .join(format!("{ICON_NAME}.png"));
        assert!(path.is_file(), "{} is missing", path.display());
    }
}

/// The desktop entry says the things a desktop needs, in the shapes the
/// specification requires.
#[test]
fn the_desktop_entry_is_the_shape_a_desktop_expects() {
    let text = std::fs::read_to_string(assets().join("pecu.desktop")).expect("the desktop entry");

    assert!(
        text.lines()
            .find(|line| !line.trim().is_empty() && !line.starts_with('#'))
            == Some("[Desktop Entry]"),
        "the group header has to be the first thing that is not a comment",
    );

    let value = |key: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(&format!("{key}=")))
            .map(str::trim)
    };

    assert_eq!(value("Type"), Some("Application"));
    assert_eq!(value("Name"), Some("Pecu"));
    assert_eq!(value("Terminal"), Some("false"));

    // The line this file exists to keep honest.
    assert_eq!(
        value("Icon"),
        Some(ICON_NAME),
        "the icon key and the theme filenames have to be the same name",
    );

    // A path here works on the machine it was written on and nowhere else.
    let exec = value("Exec").expect("Exec");
    assert!(
        !exec.starts_with('/'),
        "Exec must be resolved through PATH, not pinned to one layout: {exec}",
    );

    // The specification says these are semicolon-**terminated** lists, not
    // semicolon-separated ones, and a missing final semicolon is the classic
    // way to have the last entry silently ignored.
    for key in ["Categories", "Keywords"] {
        let list = value(key).unwrap_or_else(|| panic!("{key} is missing"));
        assert!(
            list.ends_with(';'),
            "{key} must end with a semicolon: {list}"
        );
    }
}

/// The Windows icon parses back to the sizes that went into it.
///
/// **This is not a claim that Explorer likes it.** Nothing here has been opened
/// on Windows — see `docs/LATER.md` §7. What is checked is the container: the
/// header, that every entry points inside the file, that every payload is a
/// PNG, and that the size each entry *declares* is the size its image actually
/// is. A mismatch there is the failure that renders as a smeared icon rather
/// than as an error.
#[test]
fn the_windows_icon_declares_the_sizes_it_actually_contains() {
    let bytes = std::fs::read(assets().join("pecu.ico")).expect("the icon file");
    assert!(bytes.len() > 22, "far too small to be an icon file");

    let word = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
    let long = |at: usize| {
        u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]) as usize
    };

    assert_eq!(word(0), 0, "the reserved field must be zero");
    assert_eq!(word(2), 1, "type 1 is an icon; 2 would be a cursor");
    let count = word(4) as usize;
    assert!(count >= 6, "only {count} sizes in the icon");

    let mut found = Vec::new();
    for i in 0..count {
        let entry = 6 + 16 * i;
        // Zero means 256: the field is one byte and 256 does not fit in it.
        let declared = match bytes[entry] {
            0 => 256,
            other => u32::from(other),
        };
        assert_eq!(bytes[entry + 1], bytes[entry], "entry {i} is not square");
        assert_eq!(word(entry + 6), 32, "entry {i} is not 32-bit");

        let size = long(entry + 8);
        let offset = long(entry + 12);
        assert!(
            offset + size <= bytes.len(),
            "entry {i} points past the end of the file",
        );

        let payload = &bytes[offset..offset + size];
        assert_eq!(
            &payload[..8],
            b"\x89PNG\r\n\x1a\x0a",
            "entry {i} is not a PNG",
        );
        // The IHDR's width and height, which start at byte 16 of a PNG.
        let dimension = |at: usize| {
            u32::from_be_bytes([
                payload[at],
                payload[at + 1],
                payload[at + 2],
                payload[at + 3],
            ])
        };
        assert_eq!(
            dimension(16),
            declared,
            "entry {i} declares the wrong width"
        );
        assert_eq!(
            dimension(20),
            declared,
            "entry {i} declares the wrong height"
        );
        found.push(declared);
    }

    // 256 is the one Explorer's largest view uses, and 16 is the one every
    // title bar and taskbar does.
    assert!(found.contains(&16), "no 16px entry: {found:?}");
    assert!(found.contains(&256), "no 256px entry: {found:?}");
}
