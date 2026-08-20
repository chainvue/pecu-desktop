//! What a real lightwalletd says, asked through a grpc-web proxy.
//!
//! `#[ignore]`, and it needs something running: Verus operates no public
//! grpc-web endpoint, so `scripts/grpcweb-proxy.mjs` has to be up.
//!
//!   INSECURE=1 node scripts/grpcweb-proxy.mjs &   # expired upstream cert
//!   PECU_LIGHT_URL=http://127.0.0.1:8080 \
//!     cargo test -p pecu-chain --test live_light -- --ignored --nocapture
//!
//! # Why the address is not shipped
//!
//! `verus-light` speaks grpc-web over HTTP/1.1; lightwalletd speaks native gRPC
//! over HTTP/2. `lightwalletd.verustest.net:8125` is the second kind — it sends
//! an HTTP/2 SETTINGS frame the instant a socket opens — so no amount of
//! certificate renewal would make the wallet able to read it directly. See
//! `Network::light_server`.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use pecu_chain::{LightRefused, LightServer, Network};

/// Where the proxy is, or nothing.
fn url() -> Option<String> {
    std::env::var("PECU_LIGHT_URL")
        .ok()
        .filter(|u| !u.is_empty())
}

/// The server answers, and says which chain it serves.
#[test]
#[ignore = "needs a grpc-web proxy; see the module docs"]
fn the_light_server_answers_and_says_which_chain_it_is() {
    let Some(url) = url() else {
        panic!("set PECU_LIGHT_URL — see the module docs for the proxy");
    };

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
#[ignore = "needs a grpc-web proxy; see the module docs"]
fn a_server_for_another_chain_is_refused() {
    let Some(url) = url() else {
        panic!("set PECU_LIGHT_URL — see the module docs for the proxy");
    };

    match LightServer::connect(&url, &Network::Mainnet) {
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

/// No chain ships an address, so `shipped` refuses for every one of them.
#[test]
fn nothing_is_shipped_to_connect_to() {
    for network in Network::shipped() {
        assert!(matches!(
            LightServer::shipped(&network),
            Err(LightRefused::NoServer(_)),
        ));
    }
}
