//! What a test waits on when it waits on the actor, and why it is not a clock.
//!
//! Every wait in these files has one shape: send a command, then read events
//! until one of them is the answer. What differed was the budget on that read —
//! ten, twenty or thirty seconds, written at each site by whoever wrote the
//! site — and #47 is what those figures cost. Two twenty-second waits in
//! `demo_chain.rs` elapsed on a machine at load average 26, in a file that
//! passes twelve times out of twelve when the same machine is idle.
//!
//! Raising the figures would have been the wrong repair, and the issue says so:
//! it trades an intermittent failure for a slow one and leaves the question
//! unasked. The question is what the wait is actually waiting for. It is not
//! waiting for twenty seconds to pass. It is waiting for the wallet to reach a
//! state — a balance that is not zero, a list with four identities in it, a
//! verdict that is no longer "looking up" — and the caller is the only thing
//! that knows which state that is. So the caller says so in a predicate, and
//! this reads events until one satisfies it. That is what `live_shielded_spend.rs`
//! arrived at against a real chain, where waiting on a figure rather than on the
//! shape of the state produced a wait that returned before the operation it was
//! waiting for had happened at all.
//!
//! # There is still a clock, and what it is for
//!
//! Two, and neither is a budget for the work.
//!
//! `SILENCE` is how long the core may say *nothing at all*. This is the signal
//! worth failing on, because it does not scale with the machine the way the
//! work does: the scripted chain sleeps 140 ms per request and the actor emits
//! as each read lands, so a wait that is progressing produces events throughout,
//! whether it takes two seconds or forty. Silence is a different event from
//! slowness — a dead actor, a blocking pool that will not drain.
//!
//! `GIVE_UP` is a backstop and nothing else. Silence alone cannot bound a wait,
//! because the actor has a heartbeat: `Core::run`'s idle tick fires every five
//! seconds and polls the tip every fifteen, so a wait for something that will
//! never arrive would be kept alive by the polling for as long as the process
//! lives. The failure would then be a sixty-minute job timeout naming no test,
//! which is worse than what #47 is about rather than better.
//!
//! # The measurements these two figures are sized from
//!
//! On an idle eight-core machine, `demo_chain.rs` and `currency_from_a_new_name.rs`
//! run in 19 s and 10 s. The longest single wait in them is 13.6 s — against
//! budgets of 10, 20 and 30 seconds — and the longest stretch of silence inside
//! one is 8.5 s, which is the passphrase KDF in a debug build.
//!
//! Under forty busy loops on those eight cores and `--test-threads=16`, the
//! figures before this change are 4 runs out of 4 red, failing at
//! `demo_chain.rs:995` and `:775` — the two lines the issue names. The same
//! load against this file: 4 runs out of 4 green, longest single wait 25.1 s,
//! longest silence inside one 19.9 s. So the wait that matters had already gone
//! past the twenty-second budget it replaced, and had not been quiet for a sixth
//! of `SILENCE`. That gap between the two — how long the work takes, against how
//! long the actor is silent while doing it — is the whole reason this waits on
//! the second one.

// This is a helper rather than a `#[test]`, so `allow-panic-in-tests` does not
// cover it. Panicking is right here for the reason `live_shielded_spend.rs`
// gives for its own polling helper: giving up on a wait is a failed test, and
// handing back a `Result` would let the caller carry on against a state that
// never arrived.
#![allow(clippy::panic)]

use std::time::{Duration, Instant};

use pecu_protocol::Event;
use tokio::sync::mpsc::UnboundedReceiver;

/// How long the core may say nothing at all before the wait is called stuck.
const SILENCE: Duration = Duration::from_mins(2);

/// The outer bound, for a wallet that is talking but will never say the thing.
const GIVE_UP: Duration = Duration::from_mins(5);

/// Read events until `pick` accepts one, and return what it picked.
///
/// `pick` is handed each event by value and answers `Some` for the state being
/// waited on; anything else is discarded and the read continues. It is `FnMut`
/// because two of these waits are for facts arriving in an order the test does
/// not get to assume, which means remembering which have landed.
///
/// `what` completes the sentence "waiting for …" in every failure message.
/// Failing loudly and specifically is half the point: the failure this replaces
/// reported `Elapsed(())` and nothing else, and a red tick that says only that
/// is the kind people learn to press re-run on.
pub async fn wait_for<T>(
    events: &mut UnboundedReceiver<Event>,
    what: &str,
    mut pick: impl FnMut(Event) -> Option<T>,
) -> T {
    let started = Instant::now();
    let mut seen = 0_usize;
    let mut last = String::from("nothing");
    loop {
        let Ok(received) = tokio::time::timeout(SILENCE, events.recv()).await else {
            panic!(
                "waiting for {what}: nothing at all for {SILENCE:?}, after {seen} event(s) \
                 in {:.1?}; the last was {last}",
                started.elapsed(),
            )
        };
        let Some(event) = received else {
            panic!(
                "waiting for {what}: the core stopped after {seen} event(s) in {:.1?}; \
                 the last was {last}",
                started.elapsed(),
            )
        };
        seen += 1;
        // Kept before `pick` takes ownership of the event, and truncated because
        // a view model debug-prints to pages. The variant and the head of what
        // it carries are what say whether the wallet was mid-startup or idling.
        last = head(&event);
        if let Some(picked) = pick(event) {
            return picked;
        }
        assert!(
            started.elapsed() < GIVE_UP,
            "waiting for {what}: {seen} event(s) in {:.1?}, and none of them was it; \
             the last was {last}",
            started.elapsed(),
        );
    }
}

/// The head of an event's debug output: the variant, and enough of what it
/// carries to tell two of the same variant apart.
fn head(event: &Event) -> String {
    format!("{event:?}").chars().take(120).collect()
}
