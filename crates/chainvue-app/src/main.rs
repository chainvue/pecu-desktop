//! The ChainVue desktop wallet.
//!
//! # Why there is no `#[tokio::main]`
//!
//! Slint's event loop must own the main thread — on most backends the windowing
//! system requires it, and every component has to be created on the thread the
//! loop runs on. `#[tokio::main]` would take that thread for the async runtime
//! instead.
//!
//! So the runtime is built by hand, the wallet core runs *on* it, and
//! `ui.run()` blocks the main thread as the last thing this function does.
//! Traffic between them is two channels: commands out, events back through
//! `Weak::upgrade_in_event_loop`.

// Hide the console window on Windows release builds. No effect elsewhere.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod bridge;

use chainvue_chain::{Network, Node};
use chainvue_core::{Config, Dispatcher};
use chainvue_protocol::{
    Command, ImportMaterial, PendingAction, RefreshScope, ScreenId, Secret, SendDraft,
};
use chainvue_ui::prelude::*;
use chainvue_ui::{
    Actions, AppInfo, Motion, NetworkState, SeedState, SendState, Theme, WalletState,
};
use slint::{Model, SharedString};

/// The endpoints ChainVue ships with.
///
/// Testnet first and active by default: pointing a half-finished wallet at
/// mainnet would be a choice made for the user rather than by them.
const BUILTIN_NODES: &[(&str, &str)] = &[
    ("VRSCTEST (public)", "https://api.verustest.net"),
    ("VRSC (public)", "https://api.verus.services"),
];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Held for the life of the process: the file writer is non-blocking, and
    // dropping the guard loses whatever had not been flushed.
    let _logging = init_tracing(
        &vault_path()
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_default(),
    );

    // Built by hand rather than via `#[tokio::main]`, so the main thread stays
    // free for Slint. Held for the life of the process: dropping it would abort
    // every background task.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("chainvue-worker")
        .build()?;

    tracing::info!(
        sdk_rev = chainvue_protocol::SDK_REV,
        mock = cfg!(feature = "mock"),
        "starting"
    );

    let nodes = BUILTIN_NODES
        .iter()
        .enumerate()
        .map(|(index, (label, url))| Node::builtin(u32::try_from(index).unwrap_or(0), label, url))
        .collect();

    let (dispatcher, events) = chainvue_core::start(
        runtime.handle(),
        Config {
            nodes,
            network: Network::Testnet,
            mock: cfg!(feature = "mock"),
            vault_path: vault_path(),
        },
    );

    let ui = AppWindow::new()?;
    ui.global::<AppInfo>()
        .set_sdk_rev(chainvue_protocol::SDK_REV[..8].into());
    ui.global::<NetworkState>()
        .set_mock_mode(cfg!(feature = "mock"));

    ui.global::<WalletState>()
        .set_vault_path(vault_path().display().to_string().into());
    ui.global::<AppInfo>()
        .set_log_path(log_dir().display().to_string().into());

    chainvue_ui::chart::install(&ui);
    chainvue_ui::toast::install(&ui);
    wire_actions(&ui, dispatcher.clone());
    bridge::pump(runtime.handle(), ui.as_weak(), events);

    // Ask once at startup, so the node list is not sitting at "unknown" while
    // the user wonders whether the button does anything.
    dispatcher.send(Command::ProbeNodes);

    // Shown, then focused, then run — rather than `ui.run()`, which does the
    // first and third with nothing in between.
    //
    // The keyboard shortcuts live on a focus scope wrapping the interface, and
    // Slint delivers a key to the focused item and walks *up* from there. With
    // nothing focused there is no chain to walk and no scope sees anything —
    // while Tab keeps working, because the window handles that itself after the
    // delivery loop. The scope focuses itself on `init`, but that runs when the
    // component is built and the backend sets focus up when the window is
    // shown, which is later.
    ui.show()?;
    ui.invoke_focus_shortcuts();

    let _runtime = runtime;
    slint::run_event_loop()?;
    Ok(())
}

/// Where the wallet file lives.
///
/// Per-network directories keep testnet data from ever rendering as mainnet.
fn vault_path() -> std::path::PathBuf {
    let base = std::env::var_os("CHAINVUE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| {
                std::path::PathBuf::from(home)
                    .join("Library/Application Support/com.chainvue.wallet")
            })
        })
        .unwrap_or_else(|| std::path::PathBuf::from("."));

    let dir = base.join("testnet");
    if let Err(error) = std::fs::create_dir_all(&dir) {
        tracing::warn!(%error, path = %dir.display(), "could not create the wallet directory");
    }
    dir.join("vault.json")
}

/// Turn UI callbacks into commands.
///
/// Every body does one thing and returns: convert, send, done. No `.await`, no
/// I/O, no computation — a callback that takes 40 ms is a dropped frame.
///
/// Split by subject on purpose. Everything that can be handed a secret is in
/// [`wire_wallet`] or [`wire_backup`], and nothing in [`wire_shell`] can be —
/// so the surface worth reviewing closely is two short functions rather than a
/// search through one long one.
fn wire_actions(ui: &AppWindow, dispatcher: Dispatcher) {
    wire_wallet(ui, dispatcher.clone());
    wire_backup(ui, &dispatcher);
    wire_send(ui, &dispatcher);
    wire_settings(ui, &dispatcher);
    wire_shell(ui, dispatcher);
}

/// Creating, unlocking and importing.
///
/// Each secret-bearing body moves the text into a `Secret` and returns
/// immediately. The Slint property is blanked on the `.slint` side, inside the
/// same callback — see `Onboarding.submit` and `BackupPassphrase.submit`.
/// Nothing here logs its argument.
fn wire_wallet(ui: &AppWindow, dispatcher: Dispatcher) {
    let actions = ui.global::<Actions>();

    {
        let dispatcher = dispatcher.clone();
        let weak = ui.as_weak();
        actions.on_create_wallet(move |passphrase| {
            if let Some(ui) = weak.upgrade() {
                let wallet = ui.global::<WalletState>();
                wallet.set_problem(SharedString::new());
                wallet.set_busy(true);
            }
            dispatcher.send(Command::CreateWallet {
                name: "ChainVue".to_string(),
                passphrase: Secret::new(passphrase.to_string()),
            });
        });
    }

    {
        let dispatcher = dispatcher.clone();
        let weak = ui.as_weak();
        actions.on_unlock(move |passphrase| {
            if let Some(ui) = weak.upgrade() {
                let wallet = ui.global::<WalletState>();
                wallet.set_problem(SharedString::new());
                wallet.set_busy(true);
            }
            dispatcher.send(Command::Unlock {
                passphrase: Secret::new(passphrase.to_string()),
            });
        });
    }

    {
        // Restoring. Carries two secrets — the key material and the passphrase
        // that will protect it here — and both are blanked on the `.slint` side
        // inside the same callback. See `RestoreForm.submit`.
        let dispatcher = dispatcher.clone();
        let weak = ui.as_weak();
        actions.on_restore_wallet(move |kind, material, passphrase| {
            if let Some(ui) = weak.upgrade() {
                let wallet = ui.global::<WalletState>();
                wallet.set_problem(SharedString::new());
                wallet.set_busy(true);
            }

            // Which kind is the user's explicit choice, not a guess from the
            // shape of the text: "this is a mnemonic" is what turns on the
            // checksum, and inferring it would take that decision away.
            let secret = Secret::new(material.to_string());
            let material = match kind.as_str() {
                "wif" => ImportMaterial::Wif(secret),
                "text" => ImportMaterial::Text(secret),
                _ => ImportMaterial::Phrase(secret),
            };

            dispatcher.send(Command::ImportKey {
                label: "main".to_string(),
                material,
                passphrase: Secret::new(passphrase.to_string()),
            });
        });
    }

    actions.on_lock(move || dispatcher.send(Command::Lock));
}

/// The backup screen.
///
/// `reveal-backup` carries the vault passphrase, so this function is part of
/// the surface worth reading closely too — it is separate from [`wire_wallet`]
/// only because the two were long enough together to stop being readable.
fn wire_backup(ui: &AppWindow, dispatcher: &Dispatcher) {
    let actions = ui.global::<Actions>();

    {
        // The fourth secret-bearing callback: the passphrase again, to show a
        // phrase that was never written down. Core re-runs the key derivation
        // rather than reusing the open session — see `Vault::reveal_phrase`.
        let dispatcher = dispatcher.clone();
        let weak = ui.as_weak();
        actions.on_reveal_backup(move |passphrase| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let seed = ui.global::<SeedState>();
            seed.set_problem(SharedString::new());

            // Which key is being backed up is the wallet's answer, not the
            // screen's — the UI is told the label, it does not choose one.
            let label = ui.global::<WalletState>().get_backup_key().to_string();
            if label.is_empty() {
                return;
            }

            dispatcher.send(Command::RevealBackup {
                label,
                passphrase: Secret::new(passphrase.to_string()),
            });
        });
    }

    // ── The rest of the backup screen ─────────────────────────────────────
    {
        let dispatcher = dispatcher.clone();
        actions.on_reveal_phrase(move || dispatcher.send(Command::ShowNewPhrase));
    }

    {
        let dispatcher = dispatcher.clone();
        actions.on_hide_phrase(move || dispatcher.send(Command::HideBackup));
    }

    {
        let dispatcher = dispatcher.clone();
        let weak = ui.as_weak();
        actions.on_cancel_backup(move || {
            if let Some(ui) = weak.upgrade() {
                // Overwrite the rows here, drop the phrase over there.
                chainvue_ui::seed::close(&ui);
            }
            dispatcher.send(Command::CancelBackup);
        });
    }

    {
        let dispatcher = dispatcher.clone();
        let weak = ui.as_weak();
        actions.on_confirm_phrase(move |first, second, third| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let seed = ui.global::<SeedState>();
            seed.set_problem(SharedString::new());

            // The positions come from the challenge core sent, in the order the
            // three fields were bound to it. Core checks that they are the
            // positions it asked about, so a mismatch here fails the check
            // rather than quietly grading a different question.
            let positions = seed.get_challenge();
            let typed = [first, second, third];
            if positions.row_count() != typed.len() {
                return;
            }

            let checks = positions
                .iter()
                .zip(typed.iter())
                .map(|(position, word)| {
                    (
                        u32::try_from(position).unwrap_or(0),
                        word.trim().to_string(),
                    )
                })
                .collect();

            dispatcher.send(Command::ConfirmPhrase { checks });
        });
    }
}

/// Settings.
///
/// `change-passphrase` carries two secrets and blanks both fields on the
/// `.slint` side inside the same callback — see `PassphraseChange.submit`.
fn wire_settings(ui: &AppWindow, dispatcher: &Dispatcher) {
    let actions = ui.global::<Actions>();

    {
        let dispatcher = dispatcher.clone();
        actions.on_change_passphrase(move |old, new| {
            dispatcher.send(Command::ChangePassphrase {
                old: Secret::new(old.to_string()),
                new: Secret::new(new.to_string()),
            });
        });
    }

    {
        let dispatcher = dispatcher.clone();
        actions.on_set_auto_lock(move |minutes| {
            // Zero is the UI's way of saying "never", which the protocol
            // expresses as `None`.
            let minutes = u32::try_from(minutes).unwrap_or(5);
            dispatcher.send(Command::SetAutoLockMinutes(
                (minutes > 0).then_some(minutes),
            ));
        });
    }

    {
        let dispatcher = dispatcher.clone();
        actions.on_label_address(move |address, label| {
            dispatcher.send(Command::LabelAddress {
                address: address.to_string(),
                label: label.trim().to_string(),
            });
        });
    }

    {
        let dispatcher = dispatcher.clone();
        actions.on_forget_address(move |address| {
            dispatcher.send(Command::ForgetAddress(address.to_string()));
        });
    }

    {
        // Keys. None of these carries a secret: a label is a name, and
        // generating a key needs the wallet open rather than the passphrase
        // again — the data key is what seals it.
        let dispatcher = dispatcher.clone();
        actions.on_add_key(move |label| {
            dispatcher.send(Command::AddKey {
                label: label.trim().to_string(),
            });
        });
    }

    {
        let dispatcher = dispatcher.clone();
        actions.on_rename_key(move |from, to| {
            dispatcher.send(Command::RenameKey {
                from: from.to_string(),
                to: to.trim().to_string(),
            });
        });
    }

    {
        let dispatcher = dispatcher.clone();
        actions.on_select_key(move |label| {
            dispatcher.send(Command::SetActiveKey(label.to_string()));
        });
    }

    {
        let dispatcher = dispatcher.clone();
        actions.on_set_mainnet_spend(move |on, typed| {
            // The typed word is passed straight through. Checking it here would
            // put the guard in the layer that is easiest to bypass.
            dispatcher.send(Command::SetAllowMainnetSpend {
                on,
                typed_confirmation: typed.to_string(),
            });
        });
    }
}

/// Sending.
///
/// Nothing here carries a secret. The signed transaction stays in the core; what
/// crosses this boundary is a draft on the way in and a ticket number on the way
/// back, so a compromised UI can ask for a payment to be built but cannot make
/// one happen without the confirm step, and cannot forge the bytes at all.
fn wire_send(ui: &AppWindow, dispatcher: &Dispatcher) {
    let actions = ui.global::<Actions>();

    {
        let dispatcher = dispatcher.clone();
        actions.on_validate_draft(move |to, amount| {
            dispatcher.send(Command::ValidateDraft(SendDraft {
                from_label: String::new(),
                to: to.to_string(),
                amount: amount.to_string(),
            }));
        });
    }

    {
        let dispatcher = dispatcher.clone();
        let weak = ui.as_weak();
        actions.on_prepare_send(move |to, amount| {
            if let Some(ui) = weak.upgrade() {
                let send = ui.global::<SendState>();
                send.set_problem(SharedString::new());
            }
            dispatcher.send(Command::PrepareSend(SendDraft {
                from_label: String::new(),
                to: to.to_string(),
                amount: amount.to_string(),
            }));
        });
    }

    {
        let dispatcher = dispatcher.clone();
        let weak = ui.as_weak();
        actions.on_confirm_send(move |ticket| {
            if let Some(ui) = weak.upgrade() {
                ui.global::<SendState>().set_problem(SharedString::new());
            }
            dispatcher.send(Command::ConfirmSend {
                ticket: ticket_id(ticket),
            });
        });
    }

    {
        let dispatcher = dispatcher.clone();
        actions.on_resolve_pending(move |id, action| {
            let action = match action.as_str() {
                // Resending is the only one that can pay twice, so it is named
                // for what it does rather than for what it feels like.
                "resend" => PendingAction::ResendSameBytes,
                "abandon" => PendingAction::Abandon,
                _ => PendingAction::CheckNow,
            };
            dispatcher.send(Command::ResolvePending {
                id: ticket_id(id),
                action,
            });
        });
    }

    {
        let dispatcher = dispatcher.clone();
        actions.on_cancel_send(move |ticket| {
            dispatcher.send(Command::CancelSend {
                ticket: ticket_id(ticket),
            });
        });
    }
}

/// Slint's integers are `i32`; tickets are small and count upward.
fn ticket_id(ticket: i32) -> u64 {
    u64::try_from(ticket).unwrap_or(0)
}

/// Navigation, refresh, node selection, theme — nothing that can carry a
/// secret, by construction.
fn wire_shell(ui: &AppWindow, dispatcher: Dispatcher) {
    let actions = ui.global::<Actions>();

    {
        // Applied immediately, then written down. Waiting for the core to echo
        // it back would put a round trip between pressing the control and the
        // window changing.
        let dispatcher = dispatcher.clone();
        let weak = ui.as_weak();
        actions.on_toggle_theme(move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let theme = ui.global::<Theme>();
            let dark = !theme.get_dark();
            theme.set_dark(dark);
            dispatcher.send(Command::SetAppearance {
                dark,
                reduce_motion: !ui.global::<Motion>().get_enabled(),
            });
        });
    }

    {
        let dispatcher = dispatcher.clone();
        actions.on_set_appearance(move |dark, reduce_motion| {
            dispatcher.send(Command::SetAppearance {
                dark,
                reduce_motion,
            });
        });
    }

    {
        let dispatcher = dispatcher.clone();
        actions.on_probe_nodes(move || dispatcher.send(Command::ProbeNodes));
    }

    {
        let dispatcher = dispatcher.clone();
        actions.on_select_node(move |id| {
            // Slint's integers are `i32`; node ids are small and non-negative.
            dispatcher.send(Command::SelectNode(u32::try_from(id).unwrap_or(0)));
        });
    }

    {
        // The URL is passed through untouched. Whether it is one this wallet
        // may talk to is decided in the core by the SDK's own transport, which
        // is what refuses plaintext to anything but loopback — a check the
        // screen could skip would be no check at all.
        let dispatcher = dispatcher.clone();
        actions.on_add_node(move |label, url| {
            dispatcher.send(Command::AddNode {
                url: url.to_string(),
                label: label.to_string(),
            });
        });
    }

    {
        let dispatcher = dispatcher.clone();
        actions.on_remove_node(move |id| {
            dispatcher.send(Command::RemoveNode(u32::try_from(id).unwrap_or(0)));
        });
    }

    {
        let dispatcher = dispatcher.clone();
        actions.on_refresh(move || dispatcher.send(Command::Refresh(RefreshScope::All)));
    }

    {
        let dispatcher = dispatcher.clone();
        actions.on_open_tx(move |txid| {
            dispatcher.send(Command::LoadTxDetail(txid.to_string()));
        });
    }

    {
        // Core knows how far down it has already looked; the screen only asks
        // for more. Sending a height from here would be the UI deciding where
        // the list ends.
        let dispatcher = dispatcher.clone();
        actions.on_load_older(move || {
            dispatcher.send(Command::LoadHistory {
                key: String::new(),
                before_height: None,
            });
        });
    }

    actions.on_navigate(move |screen| {
        let id = match screen.as_str() {
            "send" => ScreenId::Send,
            "receive" => ScreenId::Receive,
            "activity" => ScreenId::Activity,
            "nodes" => ScreenId::Nodes,
            "settings" => ScreenId::Settings,
            _ => ScreenId::Dashboard,
        };
        dispatcher.send(Command::ScreenEntered(id));
    });
}

/// Logging to a file, and to stderr when there is one.
///
/// # Why a file at all
///
/// A wallet started from the Finder or the Start menu has no terminal attached,
/// so stderr goes nowhere. Every line this application has ever logged about a
/// node refusing a method, a broadcast whose outcome was unknown, or a vault
/// that would not open, has been written to a stream nobody could read. The
/// first thing anybody needs when a payment goes strange is the log, and it has
/// to exist somewhere they can be pointed at.
///
/// Rotated daily. A log with no rotation is a wallet that fills a disk, which
/// is a slower and more annoying failure than the one it was written to
/// diagnose.
///
/// # What is deliberately not in it
///
/// No passphrase, no recovery phrase, no private key. That is a property of
/// what the rest of the application logs rather than of this function — see
/// `tests/log_hygiene.rs`, which sends a payment through the mock chain and
/// greps everything that came out.
///
/// Returns a guard that must be held for the life of the process: the writer is
/// non-blocking, so dropping it stops the worker and loses whatever had not
/// been flushed — including, on a crash, the lines explaining it.
fn init_tracing(dir: &std::path::Path) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::EnvFilter;

    let filter = || {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("chainvue=info,warn"))
    };

    // Still stderr, so `cargo run` behaves as it always has.
    let console = tracing_subscriber::fmt::layer()
        .with_target(false)
        .with_filter(filter());

    let Some(logs) = writable_log_directory(dir) else {
        // Not fatal. A wallet that refuses to start because it cannot write a
        // log file is a wallet that has confused its diary with its job.
        tracing_subscriber::registry().with(console).init();
        tracing::warn!(path = %dir.display(), "no writable log directory; stderr only");
        return None;
    };

    let appender = tracing_appender::rolling::daily(&logs, "chainvue.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);

    tracing_subscriber::registry()
        .with(console)
        .with(
            tracing_subscriber::fmt::layer()
                // No colour codes in a file somebody is going to open in a text
                // editor or attach to a bug report.
                .with_ansi(false)
                .with_target(false)
                .with_writer(writer)
                .with_filter(filter()),
        )
        .init();

    tracing::info!(path = %logs.display(), "logging here");
    Some(guard)
}

/// The log directory, created, or `None` if it cannot be written to.
///
/// Creating it is not enough to know it is usable: a directory can exist and be
/// unwritable, and `tracing_appender` discovers that by panicking on the first
/// line it tries to write — which would take the wallet down at startup for the
/// sake of a log file. So this writes a byte and deletes it, and the caller
/// falls back to stderr if it could not.
fn writable_log_directory(base: &std::path::Path) -> Option<std::path::PathBuf> {
    let logs = base.join("logs");
    std::fs::create_dir_all(&logs).ok()?;

    let probe = logs.join(".writable");
    std::fs::write(&probe, b"").ok()?;
    let _ = std::fs::remove_file(&probe);

    Some(logs)
}

/// Where the log files are, for the screen that has to tell somebody.
fn log_dir() -> std::path::PathBuf {
    vault_path()
        .parent()
        .map(|dir| dir.join("logs"))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::writable_log_directory;

    #[test]
    fn a_log_directory_is_created_under_the_wallet_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let logs = writable_log_directory(dir.path()).expect("a log directory");

        assert_eq!(logs, dir.path().join("logs"));
        assert!(logs.is_dir());
        // The probe cleans up after itself: a stray file in the log directory
        // would end up attached to somebody's bug report.
        assert_eq!(
            std::fs::read_dir(&logs)
                .expect("read the log directory")
                .count(),
            0,
        );
    }

    /// The failure that must not take the wallet down with it. A wallet that
    /// refuses to start because it cannot write a log file has confused its
    /// diary with its job.
    #[test]
    fn a_directory_that_cannot_be_written_to_is_refused_rather_than_fatal() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let dir = tempfile::tempdir().expect("tempdir");
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500))
                .expect("make it read-only");

            assert!(writable_log_directory(dir.path()).is_none());

            // Put it back, or the temporary directory cannot be cleaned up.
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
                .expect("restore");
        }
    }
}
