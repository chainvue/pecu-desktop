//! The one door to a window in a test, and the rule that is actually behind it.
//!
//! Nine test files in this crate build an `AppWindow`, and before this module
//! they met the same constraint four different ways:
//!
//! - a `Mutex` in `tokens.rs`, with the reason written at it;
//! - that `Mutex` copied verbatim into `balance_captions.rs`, with the same
//!   reason, which #37 does not mention and which is the clearest evidence for
//!   what it asks: the next author did copy it, and copied the wrong
//!   explanation with it;
//! - nothing at all in `accessibility.rs`, `reveal.rs` and `translation.rs`,
//!   nor in `shortcuts.rs` and `toasts.rs` on the other of the two window
//!   shapes;
//! - and a deliberate fold to a single `#[test]` in `visual.rs` and
//!   `seed_words.rs`, argued from a sentence about Slint that is not true.
//!
//! That is #37. Not the workaround, which is cheap, but four accommodations of
//! a rule nobody had written down correctly.
//!
//! # The rule, which is per thread and not per process
//!
//! A Slint platform is installed **per thread**. `set_platform` stores it in
//! `i_slint_core`'s `GLOBAL_CONTEXT`, which is a `thread_local!`, and refuses a
//! second call on the same thread — `init_no_event_loop` `expect`s on that
//! refusal, with "platform already initialized". libtest gives every `#[test]`
//! a thread of its own, including at `--test-threads=1`, so the rule at a call
//! site is: install exactly one platform, on this thread, before building a
//! window, and never a second one.
//!
//! The files used to say something stronger and false. `tokens.rs` had "the
//! testing backend is process-global: it is installed once by
//! `init_no_event_loop` and then hands out windows", and `snapshot::install`,
//! `seed_words.rs` and `visual.rs` had "Slint permits exactly one platform per
//! process". If that were true none of these files could work: `tokens.rs`
//! installs the testing backend eleven times in one binary and `shortcuts.rs`
//! installs the offscreen platform three times in one binary, and both pass.
//! What is genuinely process-global is narrower and nothing here touches it —
//! the event-loop proxy is a `OnceCell`, set only by a backend that returns one
//! from `new_event_loop_proxy`, which is `init_integration_test_with_mock_time`
//! and its system-time sibling. Those two really are once per process, and they
//! are the reason the sentence exists upstream. Neither of the two platforms
//! below offers a proxy at all.
//!
//! So the per-thread half of this module is not a precaution. It is the rule,
//! and both functions enforce it by construction: a test cannot reach a window
//! without a platform, and cannot install two.
//!
//! # The serialising half is a precaution, and here is what it rests on
//!
//! #37 reports eight of `tokens.rs`'s window tests — eleven today — hanging
//! about half the time, at 0% CPU with every thread asleep and the run never
//! ending, while `--test-threads=1` passed in eight seconds every time. The
//! `Mutex` went in then.
//!
//! That hang did not reproduce here, and the attempt is worth recording
//! because the next person should not have to repeat it. On macOS 15 (arm64,
//! 8 cores, rustc 1.95.0), at adf4e4d, against `tokens.rs` with its guard
//! deleted:
//!
//! | what | runs | hung |
//! |---|---|---|
//! | `tokens.rs` unguarded, `--test-threads=8`, 12 busy loops | 100 | 0 |
//! | `tokens.rs` unguarded, `--test-threads=32`, 24 busy loops | 20 | 0 |
//! | `tokens.rs` unguarded, `--test-threads=16`, idle | 6 | 0 |
//! | `accessibility.rs` as shipped, `--test-threads=16` | 10 | 0 |
//! | `shortcuts.rs` as shipped, `--test-threads=16`, 12 busy loops | 60 | 0 |
//!
//! And outside libtest, to take the test harness out of the question: 432
//! thread launches over two runs, each thread building, showing, querying and
//! hiding three windows in turn, 24 threads to a round and up to 24 windows
//! alive at once. 1,296 windows, and every round finished.
//!
//! So the guard stays, and it stays here rather than in `tokens.rs`. A hang one
//! person saw and another cannot provoke reads as a window that is narrower
//! than it was, not one that is shut; 126 green runs bound how often it
//! happens, and bound nothing about whether it can. What this module changes is
//! that the precaution is one lock with one argument instead of a habit the
//! next test author has to know to copy — and that dropping it later, if
//! somebody does pin the mechanism down, is one edit against one paragraph.
//!
//! The suspect worth naming for whoever picks that up: every
//! `init_no_event_loop` builds a whole `SlintContext`, and that constructor
//! scans the system's fonts — `create_collection(true)` with `system_fonts`,
//! which on macOS is a CoreText enumeration plus a directory walk, and on Linux
//! is fontconfig. Eight threads doing that at once is the most expensive and
//! the least hermetic thing these tests do, it is upstream of every window, and
//! it is where a process with no locks of its own can still end up asleep. It
//! is also exactly what the lock below serialises, since it happens inside the
//! guard.
//!
//! # What the guard costs
//!
//! Five seconds, measured interleaved run by run against the same binaries
//! built from `main` — this machine drifts by a factor of two over an hour, so
//! two timings taken an hour apart describe the machine rather than the change.
//! Medians of five paired runs of each test binary, in seconds:
//!
//! | file | before | after |
//! |---|---|---|
//! | `accessibility.rs` | 5.84 | 10.19 |
//! | `tokens.rs` | 2.46 | 2.52 |
//! | `balance_captions.rs` | 1.06 | 1.03 |
//! | `reveal.rs` | 0.41 | 0.85 |
//! | `translation.rs` | 0.28 | 0.26 |
//! | `shortcuts.rs` | 0.12 | 0.29 |
//! | `toasts.rs` | 0.07 | 0.12 |
//! | `seed_words.rs` | 0.06 | 0.06 |
//!
//! Nearly all of it is `accessibility.rs`, whose five tests each walk the whole
//! element tree and were the only ones getting real parallelism out of this.
//! The two files that already had a guard do not move, which is the check that
//! the table is measuring the lock and not the weather. Against five seconds:
//! `visual.rs` alone is seventy-seven of the same half, and what the guard
//! insures against is a sixty-minute job timeout with no test named in it.
//!
//! # Why these return `MutexGuard` and not a tidier named type
//!
//! Because `let _ = support::window_to_read();` drops the guard on the spot and
//! silently un-serialises the file, and clippy's `let_underscore_lock` catches
//! precisely that — but only while the type it is looking at is a lock guard. A
//! newtype would read better at the call site and would make that one mistake
//! invisible, which is the wrong trade for a guard whose whole job is to be
//! held.
//!
//! # What is not folded in here
//!
//! `pecu_ui::snapshot::install` stays in `src`: `examples/render_shots.rs` uses
//! it too, and an example cannot reach a test module. `window_to_draw` wraps it
//! rather than replacing it, so there is still a second door — but it is one
//! function in `src` with a comment pointing here, rather than four test files
//! each deciding for themselves.
//!
//! `visual.rs` and `seed_words.rs` keep their single-`#[test]` shape. It was
//! argued from the false sentence above, but it is right for a better reason
//! that this module does not affect: `visual.rs` compares 150-odd reference
//! images and collects every mismatch into one message, which is what makes a
//! change that moves four screens one failure to read instead of four.

// Helpers rather than `#[test]` functions, so clippy's `allow-expect-in-tests`
// does not cover them — the same reason `pecu-core/tests/support/mod.rs` spells
// out its own allow. A test that cannot get a window has nothing left to
// assert, so failing here loudly is right.
#![allow(clippy::expect_used)]
// Every window test binary compiles this module and none of them uses both
// halves of it. The alternative is an `#[allow(dead_code)]` in all nine files,
// which is the sort of line that gets copied into the tenth without being read.
#![allow(dead_code)]

use std::rc::Rc;
use std::sync::{Mutex, MutexGuard, PoisonError};

use slint::platform::software_renderer::MinimalSoftwareWindow;

/// One window on this process at a time. The module docs are the argument.
static ONE_WINDOW_AT_A_TIME: Mutex<()> = Mutex::new(());

/// Wait for this thread's turn.
///
/// `into_inner` on a poisoned lock rather than a panic: a test that failed while
/// holding this has already reported the failure worth reading, and turning
/// every test after it into a lock panic would bury it.
fn turn() -> MutexGuard<'static, ()> {
    ONE_WINDOW_AT_A_TIME
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// This thread's turn at a window whose element tree can be read and pressed.
///
/// The shape `accessibility.rs`, `reveal.rs`, `tokens.rs`, `balance_captions.rs`
/// and `translation.rs` want: `ElementQuery` walks the live tree, and
/// `invoke_accessible_default_action` runs the same `clicked` handler a pointer
/// would. Nothing is rasterised, so the font metrics are the backend's fixed
/// ones and no reference image is involved.
///
/// Hold the guard for the rest of the test — `let _turn = …`, never
/// `let _ = …`, which drops it on the spot.
pub fn window_to_read() -> MutexGuard<'static, ()> {
    let turn = turn();
    i_slint_backend_testing::init_no_event_loop();
    turn
}

/// This thread's turn at a window with a real rasteriser behind it.
///
/// The shape `shortcuts.rs`, `toasts.rs`, `seed_words.rs` and `visual.rs` want:
/// key events need a laid-out window to arrive in, and a reference image needs
/// pixels. The returned window is the one to size, redraw and read frames from.
///
/// Hold the guard for the rest of the test, the same way — `let (_turn, window)`.
pub fn window_to_draw() -> (MutexGuard<'static, ()>, Rc<MinimalSoftwareWindow>) {
    let turn = turn();
    let window = pecu_ui::snapshot::install().expect("the offscreen platform");
    (turn, window)
}
