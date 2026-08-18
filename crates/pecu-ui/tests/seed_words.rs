//! What the phrase screen does with the words it is given.
//!
//! # Its own test binary, and one `#[test]` inside it
//!
//! Slint permits exactly one platform per process and `snapshot::install()`
//! sets it, so this cannot live beside `tests/visual.rs` — a second install in
//! the same process fails. Cargo gives each integration test file its own
//! binary, which is what makes two platform-owning tests possible at all.
//!
//! And one test inside it, for the same reason plus a second: the harness runs
//! tests on separate threads, and a Slint component belongs to the thread it
//! was created on.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use pecu_protocol::SeedWordVm;
use pecu_ui::{seed, AppWindow, SeedState};
use slint::{ComponentHandle, Model};

/// Not a real mnemonic, and not twenty-four of anything: this file gets read,
/// and a checked-in BIP-39 phrase is indistinguishable at a glance from
/// someone's actual one.
fn words() -> Vec<SeedWordVm> {
    ["alpha", "bravo", "charlie", "delta", "echo", "foxtrot"]
        .iter()
        .enumerate()
        .map(|(index, word)| SeedWordVm {
            index: u32::try_from(index + 1).expect("small"),
            word: (*word).to_string(),
        })
        .collect()
}

fn rows(ui: &AppWindow) -> Vec<String> {
    let model = ui.global::<SeedState>().get_words();
    (0..model.row_count())
        .filter_map(|index| model.row_data(index))
        .map(|row| row.word.to_string())
        .collect()
}

#[test]
fn the_phrase_is_masked_except_while_it_is_being_shown() {
    let _window = pecu_ui::snapshot::install().expect("offscreen platform");
    let ui = AppWindow::new().expect("window");

    // Opening lays the grid out from the word COUNT alone. No word has been
    // sent yet, and the screen is already the right size and already covered.
    seed::open(&ui, &[3, 5], 6);
    assert_eq!(rows(&ui), vec![seed::MASK; 6]);
    assert_eq!(ui.global::<SeedState>().get_step(), "phrase");
    assert_eq!(ui.global::<SeedState>().get_challenge().row_count(), 2);

    // Held down.
    seed::reveal(&ui, &words());
    assert_eq!(
        rows(&ui),
        vec!["alpha", "bravo", "charlie", "delta", "echo", "foxtrot"],
    );

    // Released. THIS is the assertion the whole design turns on: every row is
    // overwritten, and the rows are still there. A conceal that emptied the
    // model instead would pass a "no words are visible" check while leaving
    // each `SharedString` alive for as long as anything still referenced it.
    seed::conceal(&ui);
    assert_eq!(
        rows(&ui),
        vec![seed::MASK; 6],
        "a word survived the release"
    );
    assert_eq!(
        ui.global::<SeedState>().get_words().row_count(),
        6,
        "the grid collapsed instead of being overwritten",
    );

    // Numbering survives, because the grid is the same rows throughout.
    let model = ui.global::<SeedState>().get_words();
    let numbers: Vec<i32> = (0..model.row_count())
        .filter_map(|index| model.row_data(index))
        .map(|row| row.index)
        .collect();
    assert_eq!(numbers, vec![1, 2, 3, 4, 5, 6]);

    closing_overwrites_before_it_drops(&ui);
    closing_an_unopened_screen_is_harmless(&ui);
}

/// Closing the screen empties the models — after overwriting them.
fn closing_overwrites_before_it_drops(ui: &AppWindow) {
    seed::open(ui, &[1], 6);
    seed::reveal(ui, &words());
    assert!(rows(ui).contains(&"alpha".to_string()));

    seed::close(ui);

    let state = ui.global::<SeedState>();
    assert_eq!(state.get_words().row_count(), 0);
    assert_eq!(state.get_challenge().row_count(), 0);
    assert_eq!(state.get_step(), "");
    assert_eq!(state.get_problem().code, "");
}

/// A backup that was never opened must not blow up when the screen is closed —
/// the lock path calls this unconditionally, whether or not one was open.
fn closing_an_unopened_screen_is_harmless(ui: &AppWindow) {
    seed::close(ui);
    seed::conceal(ui);
    assert_eq!(ui.global::<SeedState>().get_words().row_count(), 0);
}
