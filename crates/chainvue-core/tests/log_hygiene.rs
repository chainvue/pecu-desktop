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
//! `~/Library/Application Support/com.chainvue.wallet/testnet/logs/` is a
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

use chainvue_chain::{Network, Node};
use chainvue_core::{start, Config};
use chainvue_protocol::{Command, Event, ImportMaterial, Secret};

/// The passphrase this test uses, chosen to be unmistakable in a haystack.
const PASSPHRASE: &str = "correct-horse-battery-staple-9931";

/// From the SDK's own fixtures. A key this project has never held.
const WIF: &str = "UusoQWsobQKUkezgBJa22D9G4t9Avo6k8wD5UUxmmfAEoTN8bawc";

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
    let handle = tokio::runtime::Handle::current();
    let (dispatcher, mut events) = start(
        &handle,
        Config {
            nodes: vec![Node::builtin(0, "one", "https://example.invalid")],
            network: Network::Testnet,
            mock: false,
            vault_path: dir.path().join("vault.json"),
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

    dispatcher.send(Command::Lock);
    dispatcher.send(Command::Unlock {
        passphrase: Secret::from("also not the passphrase"),
    });
    wait_for_notice(&mut events).await;

    dispatcher.send(Command::Unlock {
        passphrase: Secret::from(PASSPHRASE),
    });
    wait_for_wallet(&mut events, |vm| !vm.locked).await;

    dispatcher.send(Command::ChangePassphrase {
        old: Secret::from("wrong again"),
        new: Secret::from("a new one"),
    });
    wait_for_notice(&mut events).await;

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
    want: impl Fn(&chainvue_protocol::WalletVm) -> bool,
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
