//! Reduce motion has to mean **all** of it.
//!
//! The mechanism is one line per animation — `enabled: Motion.enabled;` — which
//! is what makes it cheap enough to be applied everywhere. It is also what
//! makes it easy to forget on the next animation somebody adds, and a switch
//! that stops nine movements out of ten is not a switch: for someone whose
//! vestibular system is the reason it exists, the tenth is the one that matters.
//!
//! So the contract is asserted rather than remembered. This reads the `.slint`
//! sources and fails on any `animate` block that does not carry the gate.

#![allow(clippy::expect_used)]

use std::path::{Path, PathBuf};

/// Every `.slint` file under `ui/`.
fn sources() -> Vec<PathBuf> {
    fn walk(dir: &Path, into: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, into);
            } else if path.extension().is_some_and(|e| e == "slint") {
                into.push(path);
            }
        }
    }

    let mut found = Vec::new();
    walk(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("ui"),
        &mut found,
    );
    assert!(!found.is_empty(), "no .slint sources found to check");
    found
}

/// The body of an `animate` block, from its `{` to the matching `}`.
///
/// Braces are counted rather than assuming one line: an `animate` with an
/// easing in it spans three or four, and a line-based check would silently pass
/// every one of them.
fn animate_blocks(source: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    // Bytes, not chars: `find` returns a byte offset, and these sources are
    // full of em dashes and arrows, so a `Vec<char>` indexed by a byte offset
    // reads the wrong character — which silently made this miss two thirds of
    // the animations it was written to find.
    let bytes = source.as_bytes();
    let mut at = 0;

    while let Some(found) = source[at..].find("animate ") {
        let start = at + found;
        // Skip a word ending in "animate" — `reanimate`, or a comment mentioning
        // it — by requiring the character before to be whitespace or nothing.
        // Whitespace is ASCII, so a byte comparison is exact here.
        let is_word_start = start == 0
            || bytes
                .get(start.saturating_sub(1))
                .is_some_and(u8::is_ascii_whitespace);
        at = start + "animate ".len();
        if !is_word_start {
            continue;
        }

        let Some(open) = source[start..].find('{').map(|o| start + o) else {
            continue;
        };
        let mut depth = 0usize;
        let mut end = open;
        for (index, character) in source[open..].char_indices() {
            match character {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = open + index;
                        break;
                    }
                }
                _ => {}
            }
        }
        blocks.push(source[start..=end].to_string());
        at = end;
    }

    blocks
}

#[test]
fn every_animation_honours_the_reduce_motion_switch() {
    let mut ungated = Vec::new();

    for path in sources() {
        let source = std::fs::read_to_string(&path).expect("read a .slint source");
        for block in animate_blocks(&source) {
            if !block.contains("enabled:") {
                ungated.push(format!(
                    "{}\n    {}",
                    path.display(),
                    block.replace('\n', "\n    ")
                ));
            }
        }
    }

    assert!(
        ungated.is_empty(),
        "these animations run even when someone has asked for reduced motion. \
         Add `enabled: Motion.enabled;` to each:\n\n{}",
        ungated.join("\n\n"),
    );
}

/// The check above is only worth anything if it can actually see an animation.
/// A parser that found none would pass silently forever.
#[test]
fn the_check_finds_the_animations_that_are_there() {
    let total: usize = sources()
        .iter()
        .map(|path| {
            let source = std::fs::read_to_string(path).expect("read a .slint source");
            animate_blocks(&source).len()
        })
        .sum();

    assert!(
        total >= 10,
        "only {total} animate blocks were found, which means the parser has \
         stopped seeing them rather than that the interface stopped moving",
    );
}

/// And that it would notice a missing gate, rather than matching something that
/// happens to be there in every block.
#[test]
fn an_ungated_animation_would_be_caught() {
    let sample = "
        Rectangle {
            animate x { duration: 100ms; }
            animate y { duration: 100ms; enabled: Motion.enabled; }
        }
    ";

    let blocks = animate_blocks(sample);
    assert_eq!(blocks.len(), 2, "{blocks:#?}");
    assert!(!blocks[0].contains("enabled:"));
    assert!(blocks[1].contains("enabled:"));
}
