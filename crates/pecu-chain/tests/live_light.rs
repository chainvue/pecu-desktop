//! What a real lightwalletd says, asked over each dialect it might speak.
//!
//! `#[ignore]`, because these reach the network. Nothing else is needed — they
//! ask whatever this chain ships:
//!
//!   cargo test -p pecu-chain --test live_light -- --ignored --nocapture
//!
//! `PECU_LIGHT_URL` points them somewhere else — a grpc-web proxy, a
//! lightwalletd of your own, or `scripts/grpcweb-proxy.mjs` on loopback:
//!
//!   PECU_LIGHT_URL=http://127.0.0.1:9067 \
//!     cargo test -p pecu-chain --test live_light -- --ignored --nocapture
//!
//! # What these are actually guarding
//!
//! That a *named* endpoint is a *reachable* one. This wallet once shipped an
//! address it could not read, and two independent faults were each hiding the
//! other: `lightwalletd.verustest.net:8125` speaks native gRPC over HTTP/2 —
//! it sends a SETTINGS frame the instant a socket opens, which an HTTP/1.1
//! client reports as a malformed header — and it was also behind a certificate
//! that had expired. Fixing one changed nothing visible. Only connecting finds
//! that out; comparing strings never will.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use pecu_chain::{LightRefused, LightServer, Network};

/// Which server to ask: `PECU_LIGHT_URL`, or the one this chain ships.
///
/// Falling back rather than demanding the variable is what lets the whole file
/// run with no setup — which matters, because a live test nobody can run
/// without a recipe is a live test nobody runs.
fn url() -> String {
    std::env::var("PECU_LIGHT_URL")
        .ok()
        .filter(|u| !u.is_empty())
        .or_else(|| Network::Testnet.light_server().map(str::to_string))
        .expect("testnet ships a light server")
}

/// The server answers, and says which chain it serves.
#[test]
#[ignore = "connects to a real lightwalletd; see the module docs"]
fn the_light_server_answers_and_says_which_chain_it_is() {
    let url = url();
    let server = LightServer::connect(&url, &Network::Testnet).expect("connect");

    let info = server.info();
    println!("url        {}", server.url());
    println!("version    {}", info.version);
    println!("chain      {}", info.chain_name);
    println!("sapling at {}", info.sapling_activation_height);
    println!("branch id  {}", info.consensus_branch_id);
    println!("tip        {}", info.block_height);
    println!("synced to  {}", info.estimated_height);

    assert_eq!(info.chain_name, "VRSCTEST");
    assert!(info.block_height > 1_000_000, "implausible tip");
    // The one Verus-specific value on the whole shielded path.
    assert_eq!(info.consensus_branch_id, "76b809bb");

    let synced = server.synced_height().expect("synced height");
    println!("synced_height() -> {synced}");
    assert!(synced > 0);
}

/// The guard is not decoration: pointed at testnet while expecting mainnet, it
/// must refuse rather than hand back a balance from the wrong chain.
#[test]
#[ignore = "connects to a real lightwalletd; see the module docs"]
fn a_server_for_another_chain_is_refused() {
    match LightServer::connect(&url(), &Network::Mainnet) {
        Err(LightRefused::WrongNetwork {
            reported, expected, ..
        }) => {
            assert_eq!(reported, "VRSCTEST");
            assert_eq!(expected, "VRSC");
        }
        Err(other) => panic!("refused for the wrong reason: {other}"),
        Ok(_) => panic!("a VRSCTEST server was accepted as VRSC"),
    }
}

/// Every chain but testnet refuses, because nothing has been measured for them.
///
/// Testnet is deliberately absent from this loop: `shipped` **connects**, and a
/// test that reaches the network does not belong in the ordinary run. That the
/// address works is asserted above, under `#[ignore]`, which is the only place
/// it can honestly be asserted at all.
#[test]
fn a_chain_with_nothing_measured_refuses_rather_than_guessing() {
    for network in Network::shipped() {
        if matches!(network, Network::Testnet) {
            continue;
        }
        assert!(
            matches!(LightServer::shipped(&network), Err(LightRefused::NoServer(_))),
            "{} offered a light server nobody has connected to",
            network.chain_name(),
        );
    }
}

/// The one that ships is reached, not merely named.
///
/// This wallet has shipped an unreachable address once: lightwalletd's own
/// port, which speaks a protocol the transport cannot use, behind a certificate
/// that had expired. Both faults were invisible to a test that only compared
/// strings.
#[test]
#[ignore = "connects to the shipped endpoint"]
fn the_shipped_endpoint_answers_for_the_chain_it_claims() {
    let server = LightServer::shipped(&Network::Testnet).expect("connect to the shipped endpoint");
    println!("url    {}", server.url());
    println!("chain  {}", server.info().chain_name);
    assert_eq!(server.info().chain_name, "VRSCTEST");
    assert_eq!(server.info().consensus_branch_id, "76b809bb");
}

/// The native transport reaches lightwalletd's own port, with nothing between.
///
/// This is the claim the whole `grpc` module exists to make, so it is asserted
/// against the real server rather than a fixture: HTTP/2, ALPN, gRPC trailers
/// and the SDK's framing all have to line up at once, and any one of them being
/// wrong is invisible until something answers.
#[test]
#[ignore = "connects to lightwalletd.verustest.net"]
fn the_native_transport_reaches_lightwalletd_directly() {
    let url = std::env::var("PECU_LIGHT_GRPC")
        .unwrap_or_else(|_| "https://lightwalletd.verustest.net:8125".to_string());

    let transport = pecu_chain::GrpcTransport::new(&url).expect("a usable endpoint");
    let client = verus_sdk::light::LightClient::new(transport);

    let info = client
        .server_info()
        .expect("GetLightdInfo over native gRPC");
    println!("url        {url}");
    println!("version    {}", info.version);
    println!("chain      {}", info.chain_name);
    println!("branch id  {}", info.consensus_branch_id);
    println!("tip        {}", info.block_height);

    assert_eq!(info.chain_name, "VRSCTEST");
    assert_eq!(info.consensus_branch_id, "76b809bb");

    // A second call on the same connection: this is what says the pool works
    // and that the first response's trailers did not leave the stream wedged.
    let tip = client
        .latest_block()
        .expect("GetLatestBlock on a reused connection");
    println!("tip again  {}", tip.height);
    assert!(tip.height > 1_000_000, "implausible tip");
}

/// Both shapes of endpoint work, and each is recognised for what it is.
///
/// The one test that would have caught the wallet shipping an address it could
/// not read. It asserts the *pairing*, not just that each answers: a probe that
/// silently fell back to grpc-web everywhere would still pass a test that only
/// checked for a chain name.
#[test]
#[ignore = "connects to two real servers"]
fn each_endpoint_is_reached_over_the_dialect_it_actually_serves() {
    use pecu_chain::Dialect;

    for (url, expected) in [
        // lightwalletd itself, on its own port.
        ("https://lightwalletd.verustest.net:8125", Dialect::Native),
        // grpcwebproxy behind a Cloudflare tunnel — which negotiates HTTP/2 at
        // the edge, which is precisely why ALPN cannot be used to decide this.
        ("https://lwd.chainvue.io", Dialect::Web),
    ] {
        let server = LightServer::connect(url, &Network::Testnet)
            .unwrap_or_else(|e| panic!("connect to {url}: {e}"));
        println!("{url} -> {}", server.dialect().label());
        assert_eq!(server.info().chain_name, "VRSCTEST");
        assert_eq!(
            server.dialect(),
            expected,
            "{url} was reached over {}, expected {}",
            server.dialect().label(),
            expected.label()
        );
    }
}
