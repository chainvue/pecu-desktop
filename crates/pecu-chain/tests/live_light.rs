//! What the real lightwalletd says, asked on demand.
//!
//! `#[ignore]`, like every other live test here: it needs the open internet and
//! a server this project does not run. Run with
//! `cargo test -p pecu-chain --test live_light -- --ignored --nocapture`.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use pecu_chain::{LightRefused, LightServer, Network};

#[test]
#[ignore = "talks to Verus's public testnet lightwalletd"]
fn the_testnet_light_server_answers_and_says_which_chain_it_is() {
    let server = LightServer::shipped(&Network::Testnet).expect("connect to the testnet server");

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

    let synced = server.synced_height().expect("synced height");
    println!("synced_height() -> {synced}");
    assert!(synced > 0);
}

/// The guard is not decoration: pointed at testnet while expecting mainnet, it
/// must refuse rather than hand back a balance from the wrong chain.
#[test]
#[ignore = "talks to Verus's public testnet lightwalletd"]
fn a_server_for_another_chain_is_refused() {
    let url = Network::Testnet
        .light_server()
        .expect("testnet ships a server");

    match LightServer::connect(url, &Network::Mainnet) {
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

/// Mainnet ships no light server, and that must be a plain statement rather
/// than a connection attempt to a name nobody measured.
#[test]
fn mainnet_reports_no_light_server_rather_than_guessing_one() {
    assert!(Network::Mainnet.light_server().is_none());
    assert!(matches!(
        LightServer::shipped(&Network::Mainnet),
        Err(LightRefused::NoServer(chain)) if chain == "VRSC"
    ));
}
