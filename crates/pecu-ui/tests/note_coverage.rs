//! Every reason the core can name has words somewhere.
//!
//! # Why this test exists
//!
//! Because the alternative is finding out by looking. `note.slint` renders a
//! code it has no sentence for **as the code** — deliberately, so a gap is ugly
//! rather than blank — and that is a good last line of defence and a poor first
//! one: it only fires if somebody happens to reach the screen that shows it. A
//! refusal on the currency form that nobody photographs would ship reading
//! `draft-alloc-nothing`.
//!
//! It caught exactly that during the change that introduced it: `price-from-
//! notarization` was filed in the wrong chain of the two, and the markets
//! screen printed the code where the sentence should have been.
//!
//! # Why it reads the core's source rather than calling it
//!
//! `pecu-ui` cannot depend on `pecu-core` — that is the boundary
//! `dependency_boundary.rs` enforces, and it is worth more than this test. So
//! this reads the files. It is a lint over the repository, which is what it
//! would be even with a dependency: the question is not what one function
//! returns, it is whether the whole table is covered.

#![allow(clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::PathBuf;

/// Where the sentences live.
fn words() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("ui")
        .join("components")
        .join("note.slint");
    std::fs::read_to_string(path).expect("note.slint is readable")
}

/// Every code the core builds, from `NoteVm::plain("…")` and `NoteVm::with("…"`.
///
/// Also the ones the interface constructs itself — `{ code: "step-kind" }` —
/// because those need a sentence for exactly the same reason.
fn codes_emitted() -> BTreeSet<String> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .to_path_buf();

    let mut found = BTreeSet::new();
    let mut sources = Vec::new();
    collect(&root.join("pecu-core").join("src"), "rs", &mut sources);
    collect(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui"),
        "slint",
        &mut sources,
    );

    for path in sources {
        let text = std::fs::read_to_string(&path).expect("a readable source");
        // Skip the words file itself: what it *handles* is the other half of
        // this comparison, and every code in it would otherwise be its own
        // evidence that it is covered.
        if path.ends_with("note.slint") {
            continue;
        }
        for (marker, offset) in [
            ("NoteVm::plain(\"", 0),
            ("NoteVm::with(\"", 0),
            ("code: \"", 0),
        ] {
            let mut rest = text.as_str();
            while let Some(at) = rest.find(marker) {
                let after = &rest[at + marker.len() + offset..];
                match after.find('"') {
                    Some(end) => {
                        let code = &after[..end];
                        // A code is kebab-case and never empty. `code: ""`
                        // clears a note and names nothing.
                        if !code.is_empty()
                            && code
                                .chars()
                                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
                        {
                            found.insert(code.to_string());
                        }
                        rest = &after[end..];
                    }
                    None => break,
                }
            }
        }
    }
    assert!(
        found.len() > 80,
        "expected the whole table, found {found:?}"
    );
    found
}

fn collect(dir: &std::path::Path, extension: &str, into: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir).expect("a readable directory");
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, extension, into);
        } else if path.extension().is_some_and(|e| e == extension) {
            into.push(path);
        }
    }
}

#[test]
fn every_named_reason_has_words() {
    let words = words();
    let missing: Vec<String> = codes_emitted()
        .into_iter()
        .filter(|code| !words.contains(&format!("\"{code}\"")))
        .collect();

    assert!(
        missing.is_empty(),
        "{} reason{} named by the wallet and spelled nowhere — they would render \
         as their own code:\n  {}\n\nAdd a branch to `NoteText`, `NoticeTitle` or \
         `NoticeBody` in ui/components/note.slint.",
        missing.len(),
        if missing.len() == 1 { "" } else { "s" },
        missing.join("\n  "),
    );
}

/// …and nothing in the words file answers a code nobody emits.
///
/// The other direction, and it matters less — a stale branch is dead weight
/// rather than a bug on screen. It is here because dead weight in a table this
/// long is how the table stops being readable, and because a code that lost its
/// last caller is usually a feature somebody removed half of.
/// Every kebab-case literal anywhere in the core or the interface.
///
/// Wider than [`codes_emitted`] on purpose. A code does not always appear next
/// to `NoteVm::plain(` — the flow steps live in a `const [(&str, bool)]` table
/// and are wrapped one call later — and for *this* test being too generous is
/// the safe direction: it can only hide a dead branch, never invent one.
fn kebab_literals() -> BTreeSet<String> {
    let mut sources = Vec::new();
    collect(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates/")
            .join("pecu-core")
            .join("src"),
        "rs",
        &mut sources,
    );
    collect(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui"),
        "slint",
        &mut sources,
    );
    collect(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src"),
        "rs",
        &mut sources,
    );

    let mut found = BTreeSet::new();
    for path in sources {
        if path.ends_with("note.slint") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("a readable source");
        let mut rest = text.as_str();
        while let Some(open) = rest.find('"') {
            let after = &rest[open + 1..];
            let Some(end) = after.find('"') else { break };
            let literal = &after[..end];
            if literal.contains('-')
                && !literal.is_empty()
                && literal
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            {
                found.insert(literal.to_string());
            }
            rest = &after[end + 1..];
        }
    }
    found
}

#[test]
fn no_sentence_is_written_for_a_reason_nobody_names() {
    let emitted = kebab_literals();
    let words = words();

    let mut orphans = Vec::new();
    let mut rest = words.as_str();
    while let Some(at) = rest.find("note.code == \"") {
        let after = &rest[at + "note.code == \"".len()..];
        let Some(end) = after.find('"') else { break };
        let code = &after[..end];
        if !code.is_empty() && !emitted.contains(code) {
            orphans.push(code.to_string());
        }
        rest = &after[end..];
    }

    assert!(
        orphans.is_empty(),
        "{} sentence{} nothing can reach:\n  {}",
        orphans.len(),
        if orphans.len() == 1 {
            " for a reason"
        } else {
            "s for reasons"
        },
        orphans.join("\n  "),
    );
}
