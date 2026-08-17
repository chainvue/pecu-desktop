//! The UI must not be able to name a type that carries key material.
//!
//! This is the project's central security claim, and it is a claim about the
//! **dependency graph**, not about anyone's discipline: Rust will not resolve a
//! type from a crate that is not in `Cargo.toml`, so if `verus-sdk` is absent
//! from this crate's tree then `PrivateKey` is unnameable here — no review, no
//! convention, no lint required.
//!
//! Unlike the equivalent claim for `pecu-mock` (see its
//! `tests/no_network_stack.rs`, where Cargo's feature unification defeats a
//! `cargo tree` assertion), this one **is** checkable that way. Feature
//! unification can turn features on for a crate that is already in the graph;
//! it cannot put a crate into a graph that never referenced it.
//!
//! If this test fails, do not relax it. Move whatever needed the SDK type into
//! `pecu-core` and pass a view model instead.

// Clippy's `allow-expect-in-tests` covers `#[test]` functions, not the free
// helpers beside them — and this whole file is test code. Panicking when
// `cargo tree` will not run is correct: it means the test cannot answer its
// question, which must stop the run rather than pass quietly.
#![allow(clippy::expect_used)]

use std::process::Command;

/// Crates that must never appear beneath `pecu-ui`.
///
/// `verus-*` carries `PrivateKey`, `Address` and the transaction builders.
/// `pecu-keystore` decrypts. `pecu-core` holds the unlocked vault and
/// the signed-but-unsent transactions.
const FORBIDDEN: &[&str] = &[
    "verus-sdk",
    "verus-keys",
    "verus-tx",
    "verus-rpc",
    "verus-flows",
    "verus-wire",
    "pecu-keystore",
    "pecu-core",
    "pecu-chain",
    // The cryptography itself. If any of these are reachable, so is the vault.
    "argon2",
    "chacha20poly1305",
];

/// The normal-dependency tree of one crate, one crate name per line.
fn tree(package: &str) -> String {
    let output = Command::new(env!("CARGO"))
        .args([
            "tree",
            "--edges",
            "normal",
            "--package",
            package,
            "--prefix",
            "none",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("cargo tree must run");

    assert!(
        output.status.success(),
        "cargo tree failed for {package}: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Assert that none of `forbidden` appears in `package`'s tree.
fn refuse(package: &str, tree: &str, forbidden: &[&str], why: &str) {
    for name in forbidden {
        // Matched at a line start so a crate merely *mentioning* one of these
        // in its description cannot trip the test, and so the message names
        // what was actually found.
        let hit = tree
            .lines()
            .map(str::trim)
            .find(|line| line.starts_with(name));

        assert!(
            hit.is_none(),
            "`{name}` is reachable from {package} (found `{}`).\n{why}",
            hit.unwrap_or_default(),
        );
    }
}

#[test]
fn the_ui_cannot_reach_key_material() {
    let tree = tree("pecu-ui");
    assert!(
        tree.contains("pecu-protocol"),
        "the tree does not even contain pecu-protocol; this test is inert",
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
            "`{forbidden}` is reachable from pecu-ui (found `{}`).\n\
             The UI must not be able to name a type that carries key material.\n\
             Move whatever needed it into pecu-core and pass a view model.",
            hit.unwrap_or_default(),
        );
    }
}

/// The vault must not be able to reach a database.
///
/// Not a theoretical worry: the obvious way to make key material queryable is
/// to put it in SQLite, and the obvious way to start doing that is for the
/// keystore to gain a `rusqlite` dependency. It cannot, so the vault file stays
/// the only thing that holds secrets.
#[test]
fn the_keystore_cannot_reach_a_database() {
    let tree = tree("pecu-keystore");
    assert!(
        tree.contains("argon2"),
        "the keystore tree has no argon2; this test is inert",
    );

    refuse(
        "pecu-keystore",
        &tree,
        &["rusqlite", "pecu-store", "libsqlite3-sys"],
        "Key material belongs in the vault file and nowhere else. A database \
         the keystore can write to is a database key material can end up in.",
    );
}

/// And the database must not be able to reach key material, from the other side.
///
/// The store holds what you own and who you paid. It must not be able to hold
/// what would let someone spend it.
#[test]
fn the_store_cannot_reach_key_material() {
    let tree = tree("pecu-store");
    assert!(
        tree.contains("rusqlite"),
        "the store tree has no rusqlite; this test is inert",
    );

    refuse(
        "pecu-store",
        &tree,
        &[
            "verus-sdk",
            "verus-keys",
            "verus-tx",
            "pecu-keystore",
            "pecu-core",
            "argon2",
            "chacha20poly1305",
        ],
        "Nothing that can name a key may be reachable from the cache.",
    );
}
