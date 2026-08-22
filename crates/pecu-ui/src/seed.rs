//! The recovery phrase, on its way onto a screen and back off it.
//!
//! # Three rules, and each one is load-bearing
//!
//! 1. A real word is only ever written into a row while the reveal button is
//!    held. Everything else here writes bullets.
//! 2. Concealing **overwrites every row and keeps it**. Emptying the model
//!    instead would drop the rows without touching what they hold, and a
//!    `SharedString` is refcounted — Slint's copy would outlive the model
//!    wherever it had been cloned to. Keeping the rows also means the grid does
//!    not collapse and reflow every time the button is released.
//! 3. Nothing here ever joins the words. There is no point in this file at
//!    which a complete phrase exists as a single value, which is what makes an
//!    accidental log line or clipboard write a non-event rather than a
//!    catastrophe.
//!
//! # Why this lives in the UI crate rather than in the binary's bridge
//!
//! It is model manipulation, and it needs a window to be tested against. Here
//! it can be: `tests/seed_words.rs` installs the offscreen platform and asserts
//! rule 2 directly, which is not something a comment can do.

use std::rc::Rc;

use pecu_protocol::SeedWordVm;
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::{AppWindow, SeedState, SeedWord};

/// What a concealed word reads as.
///
/// A fixed six bullets regardless of the word's length. A mask that tracked the
/// length would leak it, and twenty-four word lengths narrow a 2048-word list
/// considerably.
pub const MASK: &str = "••••••";

/// Start a backup: lay the grid out, masked, and record what will be asked.
///
/// `word_count` arrives before any word does, so the screen is sized and
/// covered without ever having been handed the phrase.
pub fn open(ui: &AppWindow, positions: &[u32], word_count: u32) {
    let seed = ui.global::<SeedState>();
    let positions: Vec<i32> = positions.iter().copied().map(to_i32).collect();

    seed.set_challenge(ModelRc::from(Rc::new(VecModel::from(positions))));
    seed.set_words(ModelRc::from(Rc::new(VecModel::from(masked(word_count)))));
    seed.set_problem(crate::Note::default());
    seed.set_step("phrase".into());
}

/// Put the real words on screen.
pub fn reveal(ui: &AppWindow, words: &[SeedWordVm]) {
    let seed = ui.global::<SeedState>();
    let rows = seed.get_words();

    if rows.row_count() == words.len() {
        for (index, word) in words.iter().enumerate() {
            rows.set_row_data(index, row(word));
        }
        return;
    }

    // Only reached when the grid was never laid out — `open` sizes it before
    // any word arrives, so in practice the loop above is the path.
    seed.set_words(ModelRc::from(Rc::new(VecModel::from(
        words.iter().map(row).collect::<Vec<_>>(),
    ))));
}

/// Overwrite every word in place, keeping the grid.
pub fn conceal(ui: &AppWindow) {
    let rows = ui.global::<SeedState>().get_words();
    for index in 0..rows.row_count() {
        if let Some(existing) = rows.row_data(index) {
            if existing.word != MASK {
                rows.set_row_data(
                    index,
                    SeedWord {
                        index: existing.index,
                        word: MASK.into(),
                    },
                );
            }
        }
    }
}

/// Close the screen: overwrite, then drop.
///
/// In that order. Dropping alone would leave whatever each row held to be freed
/// whenever the last reference to it happens to go, which is not a moment
/// anyone controls.
pub fn close(ui: &AppWindow) {
    conceal(ui);

    let seed = ui.global::<SeedState>();
    seed.set_words(ModelRc::from(Rc::new(VecModel::<SeedWord>::from(
        Vec::new(),
    ))));
    seed.set_challenge(ModelRc::from(Rc::new(VecModel::<i32>::from(Vec::new()))));
    seed.set_problem(crate::Note::default());
    seed.set_step(SharedString::new());
    // Both of these are navigation, and both belong to the sitting that has
    // just ended. A label left behind would be the key the *next* passphrase is
    // sent for, which is the one mistake this indirection could make.
    seed.set_label(SharedString::new());
    seed.set_re_reading(false);
}

fn masked(count: u32) -> Vec<SeedWord> {
    (1..=count)
        .map(|index| SeedWord {
            index: to_i32(index),
            word: MASK.into(),
        })
        .collect()
}

fn row(word: &SeedWordVm) -> SeedWord {
    SeedWord {
        index: to_i32(word.index),
        word: word.word.as_str().into(),
    }
}

fn to_i32(value: u32) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}
