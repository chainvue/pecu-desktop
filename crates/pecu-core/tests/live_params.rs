//! Loading the real Sapling parameters, on whatever machine runs this.
//!
//! `#[ignore]` because it depends on the machine: it passes where a node has
//! already downloaded the ceremony files and is meaningless where none has. It
//! is also slow enough to be worth keeping out of every run — hashing 50 MB and
//! parsing the circuits takes seconds.
//!
//!   cargo test -p pecu-core --test live_params -- --ignored --nocapture

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;
use std::time::Instant;

use pecu_core::params;

/// The files a node put on this machine are the ones the SDK will prove with.
///
/// This is the check that matters before anything is built on top: the sizes
/// screen, the hashes are enforced by `verus-sapling`, and the circuits parse.
/// A wallet that gets this wrong finds out after thirty seconds of proving and
/// a rejected transaction.
#[test]
#[ignore = "needs the Sapling parameters on this machine"]
fn the_parameters_on_this_machine_are_the_ceremonys() {
    let home = PathBuf::from(std::env::var("HOME").expect("HOME"));
    let app_home = home.join(".pecu-does-not-exist");

    let Some(located) = params::find(&app_home) else {
        panic!(
            "no Sapling parameters found. Looked in:\n{}",
            params::search_paths(&app_home)
                .iter()
                .map(|p| format!("  {}", p.display()))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    };

    println!("spend   {}", located.spend.display());
    println!("output  {}", located.output.display());

    let started = Instant::now();
    let loaded = params::load(&located);
    let took = started.elapsed();

    match loaded {
        Ok(_) => println!("loaded and verified in {took:.1?}"),
        Err(e) => panic!("the parameters on this machine were refused: {e}"),
    }
}

/// The wallet's own directory is looked at last.
///
/// Asserted against the real search list rather than a synthetic one, because
/// the ordering only matters for the real one: a machine with a node must not
/// grow a second fifty-megabyte copy.
#[test]
#[ignore = "needs the Sapling parameters on this machine"]
fn a_machine_with_a_node_needs_no_download() {
    let home = PathBuf::from(std::env::var("HOME").expect("HOME"));
    let app_home = home.join(".pecu-does-not-exist");

    let located = params::find(&app_home).expect("parameters somewhere on this machine");
    assert!(
        !located.spend.starts_with(&app_home),
        "found our own copy rather than the node's: {}",
        located.spend.display(),
    );
}
