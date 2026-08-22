//! Every string a person can read goes through `@tr`.
//!
//! # Why a source scan rather than a runtime check
//!
//! Because the failure is silent in every other direction. A literal that never
//! reached `@tr` renders perfectly, reviews perfectly, and photographs
//! identically to one that did — the only difference is that it is permanently
//! English however many catalogues the project grows. `translation.rs` proves
//! the *wiring* works; nothing proved that the wiring was actually used, and by
//! the time anybody looked there were around three hundred and forty strings
//! that had missed it, four screens of them with not one `@tr` in the file.
//!
//! So this reads the `.slint` sources and fails on a literal in a
//! user-visible property that is not inside a `@tr(…)`. It is a lint, and it
//! belongs in the test suite because that is what runs.
//!
//! # The exceptions are listed, not inferred
//!
//! Some literals must **not** be translated, and "it looks short" is not the
//! rule — `Lock` is short and must be translated, `abandon abandon abandon …`
//! is longer and must not. Each one is named below with the reason, so adding
//! to this list is a decision somebody writes down rather than a regex somebody
//! widens.

#![allow(clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

/// Properties whose value is read by a person or by a screen reader.
///
/// Not an exhaustive list of Slint properties — an exhaustive list of the ones
/// this interface puts words in. `variant`, `tone`, `state`, `glyph` and `name`
/// are deliberately absent: they carry vocabulary the code branches on, and
/// translating those would break the branch rather than the sentence.
const SPOKEN: &[&str] = &[
    "text",
    "placeholder",
    "title",
    "body",
    "label",
    "accessible-label",
    "empty-note",
    "action-text",
    "back-text",
    "primary-text",
    "note",
    "cost",
];

/// Literals that are correct as they stand, and why.
///
/// The reason is not decoration. Every entry here is a claim that a translator
/// changing this string would make the interface **wrong** rather than merely
/// untranslated, and that claim should be readable by whoever adds the next one.
const ALLOWED: &[(&str, &str)] = &[
    // ── Not words ───────────────────────────────────────────────────────
    ("", "the empty string — an absence, not a message"),
    ("—", "the em dash this interface uses for “unknown”"),
    ("…", "an ellipsis standing alone as a busy state"),
    ("!", "a warning glyph, not the word"),
    ("✓", "a tick, drawn rather than said"),
    ("●", "a filled dot marking the step being worked on"),
    ("▾", "a disclosure arrow drawn as a character"),
    ("◆", "the shielded mark, which is a symbol"),
    ("↔", "the two-way arrow on a conversion row"),
    (
        "@",
        "the VerusID suffix, which is part of the identifier itself",
    ),
    ("%", "a per-cent sign standing as a field's unit"),
    // ── Numbers and shapes, not language ────────────────────────────────
    ("0.0000", "an amount field's shape, not a sentence"),
    ("0.0000 0000", "an amount field's shape, not a sentence"),
    ("20", "an example block count in a placeholder"),
    // ── Names ───────────────────────────────────────────────────────────
    ("pecu", "the product name, in the wordmark"),
    ("Pecu", "the product name, as a window title"),
    ("pecu@", "the wordmark, read aloud"),
    ("Verus SDK", "a product name in the about panel"),
    ("sdk ", "the prefix of a build identifier"),
    (
        "Space Grotesk and JetBrains Mono NL · SIL OFL 1.1",
        "a font licence notice: the names and the licence are what it says",
    ),
    (
        "https://my-node.example:27486",
        "an example URL in a placeholder",
    ),
    (
        "vrsc::identity.profile",
        "a VDXF key, which is not language",
    ),
    ("i-address", "the Verus term for the identifier form"),
    (
        "abandon abandon abandon …",
        "a BIP-39 example. The wordlist is English by specification, so a \
         translated hint would show input the field refuses",
    ),
    // Vocabulary the code branches on is not listed here at all — see
    // `without_comparisons`, which recognises it by shape rather than by name.
];

fn ui_sources() -> Vec<PathBuf> {
    fn walk(dir: &Path, into: &mut Vec<PathBuf>) {
        let entries = std::fs::read_dir(dir).expect("the ui directory is readable");
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
        Path::new(env!("CARGO_MANIFEST_DIR")).join("ui").as_path(),
        &mut found,
    );
    found.sort();
    assert!(
        found.len() > 10,
        "expected the whole interface, found {found:?}"
    );
    found
}

/// Literals being **compared** rather than shown, removed.
///
/// `kind == "login" ? @tr("Signed in") : …` has two strings in it and only one
/// of them is a message. The other decides which branch runs, and a catalogue
/// that translated it would not produce a bad sentence — it would produce the
/// wrong branch, silently, which is a worse bug and a much quieter one.
///
/// This is the rule the vocabulary exemptions used to be a list of. What is
/// left in `ALLOWED` are the ones that really are shown and really should not
/// change.
fn without_comparisons(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let comparison = chars[i..].starts_with(&['=', '=']) || chars[i..].starts_with(&['!', '=']);
        if comparison {
            out.push_str("==");
            i += 2;
            // Skip whatever spacing, then the literal it is compared against.
            while i < chars.len() && chars[i] == ' ' {
                i += 1;
            }
            if i < chars.len() && chars[i] == '"' {
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    i += 1;
                }
                i += 1;
            }
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// A `Note`'s own `code:` field, removed.
///
/// `label: { code: "step-kind", args: [] }` builds a named reason, and the
/// name is not a message — `note.slint` turns it into one. Recognised by shape
/// for the same reason comparisons are: a list of every code the interface
/// constructs would be a list somebody has to remember to add to.
fn without_note_codes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("code:") {
        out.push_str(&rest[..at]);
        out.push_str("code:");
        let after = &rest[at + "code:".len()..];
        let trimmed = after.trim_start();
        let skipped = after.len() - trimmed.len();
        if let Some(stripped) = trimmed.strip_prefix('"') {
            match stripped.find('"') {
                Some(end) => rest = &stripped[end + 1..],
                None => rest = stripped,
            }
        } else {
            out.push_str(&after[..skipped]);
            rest = trimmed;
        }
    }
    out.push_str(rest);
    out
}

/// Everything inside `@tr(…)`, removed — so what is left is what escaped it.
///
/// Not a parser. `@tr` calls do not nest and their arguments are property
/// paths rather than further strings, so scanning forward to the matching
/// bracket is enough and being approximate here fails **towards** reporting a
/// string rather than towards missing one.
fn without_translated(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let bytes: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(&['@', 't', 'r', '(']) {
            let mut depth = 0;
            while i < bytes.len() {
                if bytes[i] == '(' {
                    depth += 1;
                } else if bytes[i] == ')' {
                    depth -= 1;
                    if depth == 0 {
                        i += 1;
                        break;
                    }
                }
                i += 1;
            }
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

/// The string literals on a line, in order.
fn literals(line: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = line;
    while let Some(open) = rest.find('"') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('"') else {
            break;
        };
        found.push(after[..close].to_string());
        rest = &after[close + 1..];
    }
    found
}

/// What a line is, for the scan.
enum Line<'a> {
    /// A property assignment. Carries the property and the value after the
    /// colon — the value only, because a struct literal on one line puts field
    /// names in quotes right beside a `label:` and those are not sentences.
    Assignment {
        property: &'a str,
        value: &'a str,
    },
    /// A `?`, `:` or `|` continuing whatever the last assignment was.
    Continuation(&'a str),
    Other,
}

fn classify(line: &str) -> Line<'_> {
    let trimmed = line.trim_start();
    if trimmed.starts_with("//") {
        return Line::Other;
    }
    if trimmed.starts_with('?') || trimmed.starts_with(':') || trimmed.starts_with('|') {
        return Line::Continuation(trimmed);
    }
    // `name: value` at the head of the line. Bounded to an identifier so a
    // ternary's own `:` further along cannot look like one.
    let head: String = trimmed
        .chars()
        .take_while(|c| c.is_ascii_lowercase() || *c == '-')
        .collect();
    if head.is_empty() {
        return Line::Other;
    }
    match trimmed[head.len()..].strip_prefix(':') {
        Some(value) => Line::Assignment {
            property: &trimmed[..head.len()],
            value,
        },
        None => Line::Other,
    }
}

#[test]
fn every_readable_string_is_translatable() {
    let allowed: std::collections::HashSet<&str> =
        ALLOWED.iter().map(|(literal, _)| *literal).collect();
    let mut escaped: Vec<String> = Vec::new();

    for path in ui_sources() {
        let source = std::fs::read_to_string(&path).expect("a readable source");
        // Track whether we are inside a ternary that began on a spoken
        // property, so its continuation lines count too.
        // Whether the assignment still being read puts words on screen. A new
        // assignment always replaces it — without that, a `text:` ternary
        // spilling over several lines makes the `color:` ternary under it look
        // like part of the sentence, and every tone word in the file is
        // reported as untranslated.
        let mut spoken_block = false;
        let mut statement = String::new();
        let mut began = 0;
        for (number, line) in source.lines().enumerate() {
            match classify(line) {
                Line::Assignment { property, value } => {
                    spoken_block = SPOKEN.contains(&property);
                    if spoken_block {
                        statement.clear();
                        statement.push_str(value);
                        began = number + 1;
                    }
                }
                Line::Continuation(rest) if spoken_block => {
                    statement.push(' ');
                    statement.push_str(rest);
                }
                Line::Continuation(_) | Line::Other => continue,
            }

            // Whole statement, not line by line: `@tr` takes a plural form as
            // `@tr("one" | "many" % n)` and that runs over two lines, so a
            // per-line strip sees the second half outside any `@tr(` and calls
            // a translated string untranslated.
            if !line.trim_end().ends_with(';') {
                continue;
            }
            spoken_block = false;

            let bare = without_note_codes(&without_comparisons(&without_translated(&statement)));
            for literal in literals(&bare) {
                if allowed.contains(literal.as_str()) {
                    continue;
                }
                escaped.push(format!(
                    "{}:{began}  {literal:?}",
                    path.file_name().unwrap_or_default().to_string_lossy(),
                ));
            }
            statement.clear();
        }
    }

    assert!(
        escaped.is_empty(),
        "{} string{} a person can read never reach `@tr`, so no catalogue can \
         translate them:\n  {}\n\nWrap them, or — if a translator changing one \
         would make the interface wrong — add it to `ALLOWED` with the reason.",
        escaped.len(),
        if escaped.len() == 1 { "" } else { "s" },
        escaped.join("\n  "),
    );
}

/// Every exception carries a reason, and no two say the same literal twice.
#[test]
fn the_exceptions_are_explained_and_unique() {
    let mut seen = std::collections::HashSet::new();
    for (literal, why) in ALLOWED {
        assert!(
            why.len() > 10,
            "{literal:?} is exempt for {why:?}, which is not a reason",
        );
        assert!(seen.insert(literal), "{literal:?} is listed twice");
    }
}
