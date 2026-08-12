//! The UI must not be able to name a type that carries key material.
//!
//! This is the project's central security claim, and it is a claim about the
//! **dependency graph**, not about anyone's discipline: Rust will not resolve a
//! type from a crate that is not in `Cargo.toml`, so if `verus-sdk` is absent
//! from this crate's tree then `PrivateKey` is unnameable here — no review, no
//! convention, no lint required.
//!
//! Unlike the equivalent claim for `chainvue-mock` (see its
//! `tests/no_network_stack.rs`, where Cargo's feature unification defeats a
//! `cargo tree` assertion), this one **is** checkable that way. Feature
//! unification can turn features on for a crate that is already in the graph;
//! it cannot put a crate into a graph that never referenced it.
//!
//! If this test fails, do not relax it. Move whatever needed the SDK type into
//! `chainvue-core` and pass a view model instead.

use std::process::Command;

/// Crates that must never appear beneath `chainvue-ui`.
///
/// `verus-*` carries `PrivateKey`, `Address` and the transaction builders.
/// `chainvue-keystore` decrypts. `chainvue-core` holds the unlocked vault and
/// the signed-but-unsent transactions.
const FORBIDDEN: &[&str] = &[
    "verus-sdk",
    "verus-keys",
    "verus-tx",
    "verus-rpc",
    "verus-flows",
    "verus-wire",
    "chainvue-keystore",
    "chainvue-core",
    "chainvue-chain",
    // The cryptography itself. If any of these are reachable, so is the vault.
    "argon2",
    "chacha20poly1305",
];

#[test]
fn the_ui_cannot_reach_key_material() {
    let output = Command::new(env!("CARGO"))
        .args([
            "tree",
            "--edges",
            "normal",
            "--package",
            "chainvue-ui",
            "--prefix",
            "none",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("cargo tree must run");

    assert!(
        output.status.success(),
        "cargo tree failed: {}",
        String::from_utf8_lossy(&output.stderr),
    );

    let tree = String::from_utf8_lossy(&output.stdout);
    assert!(
        tree.contains("chainvue-protocol"),
        "the tree does not even contain chainvue-protocol; this test is inert",
    );

    for forbidden in FORBIDDEN {
        // Match at a line start so that a crate merely *mentioning* one of
        // these in its description cannot trip the test, and so `verus-tx`
        // does not match inside `verus-tx-primitives` misleadingly — either
        // way it is a real hit, but the message should name what was found.
        let hit = tree
            .lines()
            .map(str::trim)
            .find(|line| line.starts_with(forbidden));

        assert!(
            hit.is_none(),
            "`{forbidden}` is reachable from chainvue-ui (found `{}`).\n\
             The UI must not be able to name a type that carries key material.\n\
             Move whatever needed it into chainvue-core and pass a view model.",
            hit.unwrap_or_default(),
        );
    }
}
