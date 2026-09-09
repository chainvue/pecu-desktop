//! Nothing secret reaches the log.
//!
//! # Why this is a test and not a rule
//!
//! Because the leak, when it happens, will not be a `tracing::info!` with a
//! passphrase in it. Nobody writes that. It will be an error type that gained a
//! field, printed by a `%error` three call sites away — or a `Debug` derive
//! added to a struct that turned out to hold a phrase, logged by a catch-all
//! arm that was there for something else. Every one of those is invisible in
//! review and obvious in a file.
//!
//! So this drives the paths that are *handed* secret material and reads back
//! everything the application said while it did.
//!
//! Logs go to a file now, which is the whole reason this matters: a line on
//! stderr that nobody could see was a small problem. A line in
//! `~/Library/Application Support/com.pecu.wallet/testnet/logs/` is a
//! recovery phrase on somebody's disk, in plain text, in a directory they will
//! cheerfully attach to a bug report.

// This whole file is test code. Panicking on a fixture that did not behave is
// the correct outcome — it means the test itself is wrong, which should stop
// the run loudly rather than pass quietly. `too_many_lines` is allowed for the
// one test function: it is a sequence of steps through the application, and
// splitting it into helpers would put the thing being tested in three places.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::too_many_lines
)]

use std::io::Write;
use std::sync::{Arc, Mutex};

use pecu_chain::{Network, Node};
use pecu_core::{start, Config};
use pecu_protocol::{Command, Event, ImportMaterial, Secret};

/// The passphrase this test uses, chosen to be unmistakable in a haystack.
const PASSPHRASE: &str = "correct-horse-battery-staple-9931";

/// From the SDK's own fixtures. A key this project has never held.
const WIF: &str = "UusoQWsobQKUkezgBJa22D9G4t9Avo6k8wD5UUxmmfAEoTN8bawc";

/// A salt with a shape nothing else produces.
///
/// Consecutive bytes, so a leak through any `Debug` of the reservation prints
/// `200, 201, 202, 203` — a run that cannot occur by accident in prose, a hash,
/// or a formatted amount.
const SALT: [u8; 32] = [
    200, 201, 202, 203, 204, 205, 206, 207, 208, 209, 210, 211, 212, 213, 214, 215, 216, 217, 218,
    219, 220, 221, 222, 223, 224, 225, 226, 227, 228, 229, 230, 231,
];

/// A phrase with a broken checksum, so the import refuses it — which is the
/// path that formats a message about it.
const BAD_PHRASE: &str = "abandon abandon abandon abandon abandon abandon \
                          abandon abandon abandon abandon abandon abandon";

/// Everything the application wrote, from every thread.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Captured {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().expect("the log buffer")).to_string()
    }
}

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .expect("the log buffer")
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn no_secret_reaches_the_log() {
    let sink = Captured::default();

    // Global, not thread-local: the core does its work on blocking threads, and
    // a thread-local subscriber would capture the actor's lines and miss every
    // line written by the code that actually touches a key.
    //
    // At TRACE, because the question is not "what do we log by default" — it is
    // "what could this ever say". A leak behind `RUST_LOG=debug` is still a
    // leak, and it is the setting somebody turns on precisely when things have
    // gone wrong.
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_writer(sink.clone())
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .finish(),
    )
    .expect("this test binary sets the subscriber once");

    let dir = tempfile::tempdir().expect("tempdir");

    // A reservation on disk before the core starts, so it is picked up exactly
    // as a resumed registration would be. Without this the salt would never
    // exist and the assertions about it at the bottom would pass on nothing.
    //
    // **Where** matters as much as what, and this test used to get it wrong: it
    // wrote `registration.json` at the top of the home directory, which is
    // where it lived before `Paths` gave each chain its own folder. The core
    // has read `<home>/<chain>/registration.json` since, so nothing was picked
    // up, the salt never entered the running application, and the assertions at
    // the bottom passed against a log that had never been near one — the exact
    // failure the comment above warns about. The layout is asked, never
    // assumed.
    let paths =
        pecu_core::paths::Paths::new(dir.path().to_path_buf(), &Network::Testnet, false);
    plant_a_reservation(&paths.registration());
    plant_a_launch(&paths.launch());

    let handle = tokio::runtime::Handle::current();
    let (dispatcher, mut events) = start(
        &handle,
        Config {
            nodes: vec![Node::builtin(0, "one", "https://example.invalid")],
            network: Network::Testnet,
            mock: false,
            home: dir.path().to_path_buf(),
        },
    );

    // ── Create, and read the phrase out ─────────────────────────────────
    dispatcher.send(Command::CreateWallet {
        name: "hygiene".to_string(),
        passphrase: Secret::from(PASSPHRASE),
    });

    let positions = loop {
        match events.recv().await {
            Some(Event::PhraseChallenge { positions, .. }) => break positions,
            Some(_) => {}
            None => panic!("the core stopped before announcing a challenge"),
        }
    };

    dispatcher.send(Command::ShowNewPhrase);
    let words: Vec<String> = loop {
        match events.recv().await {
            Some(Event::SeedWords(words)) if !words.is_empty() => {
                break words.into_iter().map(|word| word.word).collect();
            }
            Some(_) => {}
            None => panic!("the core stopped before sending the words"),
        }
    };
    assert_eq!(words.len(), 24);

    // ── Then every path that is handed a secret and refuses ─────────────
    //
    // The refusals matter more than the successes: a failure is where an error
    // gets formatted, and an error that quotes its input is the classic way key
    // material ends up in a log.

    dispatcher.send(Command::ConfirmPhrase {
        checks: positions
            .iter()
            .map(|p| (*p, "wrong".to_string()))
            .collect(),
    });
    wait_for_confirmation(&mut events).await;

    dispatcher.send(Command::RevealBackup {
        label: "main".to_string(),
        passphrase: Secret::from("not the passphrase"),
    });
    wait_for_notice(&mut events).await;

    dispatcher.send(Command::ImportKey {
        label: "second".to_string(),
        material: ImportMaterial::Phrase(Secret::from(BAD_PHRASE)),
        passphrase: Secret::from(PASSPHRASE),
    });
    wait_for_notice(&mut events).await;

    dispatcher.send(Command::ImportKey {
        label: "third".to_string(),
        material: ImportMaterial::Wif(Secret::from(WIF)),
        passphrase: Secret::from(PASSPHRASE),
    });
    wait_for_wallet(&mut events, |vm| vm.keys.len() == 2).await;

    // A key that never had words, asked for its words. The refusal names the
    // key, and a message that quotes one of its inputs is how the next one
    // learns to quote the input that matters.
    dispatcher.send(Command::RevealBackup {
        label: "third".to_string(),
        passphrase: Secret::from(PASSPHRASE),
    });
    wait_for_notice(&mut events).await;

    // ── And the reveal that works ───────────────────────────────────────
    //
    // Not a refusal, and the only path here that is not. It is the route the
    // keys screen offers for the rest of the wallet's life: the phrase comes
    // back out of the vault, into the core's `Backup`, and onto a screen, and
    // every one of those hops is somewhere a `Debug` could turn up. Until this
    // existed, the only reveal this test drove was one that failed before it
    // had a phrase to leak.
    dispatcher.send(Command::RevealBackup {
        label: "main".to_string(),
        passphrase: Secret::from(PASSPHRASE),
    });
    loop {
        match events.recv().await {
            Some(Event::PhraseChallenge { .. }) => break,
            Some(Event::Notice(notice)) => {
                panic!("the phrase could not be read again: {}", notice.message.code)
            }
            Some(_) => {}
            None => panic!("the core stopped before showing the phrase again"),
        }
    }
    dispatcher.send(Command::ShowNewPhrase);
    let again: Vec<String> = loop {
        match events.recv().await {
            Some(Event::SeedWords(words)) if !words.is_empty() => {
                break words.into_iter().map(|word| word.word).collect();
            }
            Some(_) => {}
            None => panic!("the core stopped before sending the words again"),
        }
    };
    // `assert!` rather than `assert_eq!`, in the one test whose whole subject
    // is phrase words not reaching a log. A failing `assert_eq!` prints both
    // sides, and both sides here are 24 recovery words unwrapped to `String` —
    // so the regression this catches would be reported by writing the wallet's
    // phrase twice into a build log that anybody can read.
    assert!(again == words, "the same key gave different words");
    dispatcher.send(Command::HideBackup);
    dispatcher.send(Command::CancelBackup);

    dispatcher.send(Command::Lock);
    dispatcher.send(Command::Unlock {
        passphrase: Secret::from("also not the passphrase"),
    });
    wait_for_notice(&mut events).await;

    dispatcher.send(Command::Unlock {
        passphrase: Secret::from(PASSPHRASE),
    });
    wait_for_wallet(&mut events, |vm| !vm.locked).await;

    // The NEW one clears `MIN_PASSPHRASE_CHARS` on purpose. The vault judges
    // the new passphrase before it verifies the old one, so a short new one
    // would be refused a step early and `wrong again` would never reach the
    // derivation — which is the exact path this case exists to walk.
    dispatcher.send(Command::ChangePassphrase {
        old: Secret::from("wrong again"),
        new: Secret::from("a new one entirely"),
    });
    wait_for_notice(&mut events).await;

    // ── And the name reservation, whose salt is the newest secret here ──
    //
    // Written to disk before the core starts, so it is picked up the way a
    // resumed registration would be — and then every path that touches it runs:
    // the tick poller, the view the screen is built from, and the error
    // formatting when the unreachable node refuses.
    dispatcher.send(Command::AbandonRegistration);
    dispatcher.send(Command::CheckName("hygiene".to_string()));
    for _ in 0..3 {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(200), events.recv()).await;
    }

    dispatcher.send(Command::Shutdown);

    // ── What came out ───────────────────────────────────────────────────
    let log = sink.text();
    assert!(
        !log.is_empty(),
        "nothing was captured, so this test proves nothing"
    );

    assert!(
        !log.contains(PASSPHRASE),
        "the vault passphrase reached the log",
    );
    assert!(!log.contains(WIF), "a private key reached the log");

    // Individual recovery words are ordinary English — `about`, `absent`,
    // `across` — and asserting on one alone would fail against prose. Three in
    // a row is not prose, and three in a row is also what a leak looks like:
    // nothing logs one word of a phrase.
    for window in words.windows(3) {
        let run = window.join(" ");
        assert!(
            !log.contains(&run),
            "part of a recovery phrase reached the log: it contained a three-word run"
        );
    }

    // And the joined form, which is the object that must never exist above the
    // vault in the first place.
    assert!(
        !log.contains(&words.join(" ")),
        "a whole recovery phrase reached the log",
    );

    // The refused phrase was handed in as free text, so it is not a secret this
    // wallet holds — but it is still key material somebody typed, and a message
    // quoting the input is how the *next* one gets quoted too.
    assert!(
        !log.contains(BAD_PHRASE),
        "the phrase somebody typed was quoted back into the log",
    );

    // The registration salt.
    //
    // The newest secret in this application and the one with the shortest
    // history of being handled carefully: it cannot be recovered from the
    // chain, so a leak plus a lost file is a commitment fee nobody can redeem —
    // and a leak on its own lets somebody else claim the name being reserved.
    //
    // Asserted in both spellings it could reach a log in: the `Debug` of a byte
    // array, and hex.
    let as_debug = SALT
        .iter()
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    assert!(
        !log.contains(&as_debug[..24]),
        "a name reservation's salt reached the log",
    );
    assert!(
        !log.contains(&hex::encode(SALT)[..32]),
        "a name reservation's salt reached the log as hex",
    );
}

/// Write a currency waiting to be defined, so the resume path runs.
///
/// # What is being checked, given the record holds no secret
///
/// That it stays that way. The record carries a **key label**, not a key —
/// which is the design, and a design nothing enforces: the launch it resumes is
/// signed with the key that label names, and the obvious convenience of holding
/// the key alongside the decision would put private material into a file that
/// is written on a best-effort path, and into every `Debug` of it.
///
/// So this plants one, lets the core open it and report it, and the assertions
/// at the bottom read the log for the key material that is in the same wallet.
fn plant_a_launch(path: &std::path::Path) {
    let record = pecu_core::launch::Record {
        identity: "hygiene@".to_string(),
        key_label: "main".to_string(),
        step: pecu_core::launch::Step::ReadyToDefine,
        draft: pecu_protocol::CurrencyDraft {
            kind: "token".to_string(),
            identity: String::new(),
            new_name: "hygiene".to_string(),
            mintable: true,
            start_delay: "20".to_string(),
            reserves: Vec::new(),
            preallocations: Vec::new(),
        },
    };
    std::fs::write(
        path,
        serde_json::to_string(&record).expect("serialize the launch record"),
    )
    .expect("write the launch record");
}

/// Write a name reservation carrying [`SALT`] into the wallet directory.
///
/// Built through the SDK rather than hand-assembled, so what lands on disk is
/// the real shape — including whatever fields a future version adds, which is
/// precisely what a hand-written fixture would stop covering.
fn plant_a_reservation(path: &std::path::Path) {
    use verus_flows::testing::ScriptedReader;
    use verus_sdk::verus_keys::PrivateKey;

    let key = PrivateKey::from_bytes(&[7u8; 32], true).expect("a fixed scalar is a key");
    let reader = ScriptedReader::new(1_000_000)
        .with_utxo(&key.address().to_string(), 999_000, 200 * 100_000_000)
        .with_policy(verus_sdk::network::CurrencyPolicy {
            currency_id: "iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq".into(),
            name: "VRSCTEST".into(),
            id_registration_fee: verus_sdk::money::Amount::from_sat(100 * 100_000_000),
            id_referral_levels: 3,
            id_import_fee: verus_sdk::money::Amount::ZERO,
            currency_registration_fee: verus_sdk::money::Amount::ZERO,
            proof_protocol: 1,
        });

    let pending = verus_flows::prepare_registration_with_salt(
        &reader,
        &key,
        "hygiene",
        &verus_flows::RegistrationOptions::default(),
        SALT,
    )
    .expect("the reservation builds");
    assert_eq!(pending.reservation.salt, SALT, "the fixture lost its salt");

    // `Committed`, so the tick poller actually runs against it — a `Reserved`
    // one is skipped, and the polling path is where a `Debug` of the whole
    // value would most plausibly appear.
    let record = pecu_core::registration::Record {
        name: "hygiene".to_string(),
        key_label: "main".to_string(),
        step: pecu_core::registration::Step::Committed,
        pending,
    };
    std::fs::write(path, serde_json::to_string(&record).expect("serialize")).expect("write");
}

async fn wait_for_notice(events: &mut tokio::sync::mpsc::UnboundedReceiver<Event>) {
    for _ in 0..40 {
        match events.recv().await {
            Some(Event::Notice(_)) => return,
            Some(_) => {}
            None => panic!("the core stopped before saying anything"),
        }
    }
    panic!("no notice arrived");
}

async fn wait_for_confirmation(events: &mut tokio::sync::mpsc::UnboundedReceiver<Event>) {
    for _ in 0..40 {
        match events.recv().await {
            Some(Event::PhraseConfirmed(_)) => return,
            Some(_) => {}
            None => panic!("the core stopped before answering"),
        }
    }
    panic!("no answer arrived");
}

async fn wait_for_wallet(
    events: &mut tokio::sync::mpsc::UnboundedReceiver<Event>,
    want: impl Fn(&pecu_protocol::WalletVm) -> bool,
) {
    for _ in 0..60 {
        match events.recv().await {
            Some(Event::Wallet(vm)) if want(&vm) => return,
            Some(_) => {}
            None => panic!("the core stopped before reporting the wallet"),
        }
    }
    panic!("the wallet never reached the state this test was waiting for");
}
