//! Mock mode must not be able to reach a network.
//!
//! # Why this checks the source and not `cargo tree`
//!
//! The obvious test is "assert `ureq` is absent from `cargo tree -p
//! pecu-mock`", and this crate's manifest is written to make that true: it
//! takes `verus-rpc` **without** the `http` feature.
//!
//! Inside this workspace it is false anyway. Cargo unifies features across
//! normal dependencies, and `pecu-chain` needs `verus-rpc/http` for the
//! live client, so `verus-rpc` is compiled with `HttpTransport` present and
//! `ureq` appears in this crate's tree too. A `cargo tree` assertion here would
//! fail immediately — or, worse, be "fixed" by loosening it until it passed and
//! proved nothing.
//!
//! So this checks the property that holds in every build: **no source file in
//! this crate names a transport, a URL, or a socket.** `MockChain` answers from
//! an in-memory script; it holds no client, so reaching the network is not
//! refused, it is absent. Combined with `broadcasting_always_fails` in the unit
//! tests, that is the real guarantee.

use std::path::Path;

/// Things that, appearing in this crate's source, would mean it had gained a
/// way to talk to something.
const FORBIDDEN: &[&str] = &[
    "HttpTransport",
    "GrpcWebTransport",
    "ureq",
    "reqwest",
    "TcpStream",
    "UdpSocket",
    "std::net",
];

#[test]
fn no_source_file_names_a_transport() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut checked = 0usize;

    for entry in std::fs::read_dir(&src).expect("pecu-mock/src must exist") {
        let path = entry.expect("readable dir entry").path();
        if path.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("readable source file");
        checked += 1;

        for needle in FORBIDDEN {
            // The doc comment explains why each of these is absent, so a match
            // inside a comment is expected and is not evidence of a transport.
            // Compare against code lines only.
            let in_code = text.lines().any(|line| {
                let trimmed = line.trim_start();
                !trimmed.starts_with("//") && !trimmed.starts_with('*') && line.contains(needle)
            });
            assert!(
                !in_code,
                "{} names `{needle}` in code — mock mode must hold no client",
                path.display(),
            );
        }
    }

    assert!(
        checked > 0,
        "no source files were checked; the test is inert"
    );
}

/// The manifest still declines `verus-rpc/http`. That buys nothing inside this
/// workspace (see the module docs) but is the correct declaration of intent and
/// does hold if this crate is ever built on its own, so it should not be
/// silently dropped.
#[test]
fn the_manifest_still_declines_the_http_feature() {
    let manifest =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
            .expect("readable manifest");

    assert!(
        manifest.contains("verus-rpc"),
        "this crate is expected to depend on verus-rpc directly, so it can decline `http`",
    );
    assert!(
        !manifest.contains(r#"features = ["http"]"#),
        "pecu-mock must not ask for verus-rpc's http feature",
    );
}
