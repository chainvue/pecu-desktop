//! The Pecu desktop wallet.
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
mod instance;

use pecu_chain::Network;
use pecu_core::paths::Paths;
use pecu_core::{Config, Dispatcher};
use pecu_protocol::{
    Command, ImportMaterial, PendingAction, RefreshScope, ScreenId, Secret, SendDraft,
};
use pecu_ui::prelude::*;
use pecu_ui::{
    Actions, AppInfo, Motion, NetworkState, SeedState, SendState, Theme, WalletState,
};
use slint::{Model, ModelRc, SharedString, VecModel};
use std::ffi::OsString;
use std::path::PathBuf;
use std::rc::Rc;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let home = home_dir();

    // Held for the life of the process: the file writer is non-blocking, and
    // dropping the guard loses whatever had not been flushed.
    //
    // At the home directory rather than inside a chain's, because the chain can
    // change while the process runs and tracing is installed exactly once. A log
    // that stopped following the wallet halfway through a session would be worse
    // than one covering both chains.
    let _logging = init_tracing(&home);

    // Before anything is opened. `instance::acquire` explains what two copies
    // of this application do to the three files that record money already
    // spent; the short version is that the second one wins and the first one's
    // record is gone.
    //
    // Bound to a named variable, held to the end of `main`: the lock lives as
    // long as the file descriptor, and `_` would drop it here.
    let _instance = match instance::acquire(&home) {
        Ok(guard) => Some(guard),
        Err(instance::Busy::AlreadyRunning) => return already_running(),
        // The lock could not be taken at all, which is not a reason to refuse
        // somebody their wallet — see `instance::acquire`.
        Err(instance::Busy::NoLockFile) => None,
    };

    // Built by hand rather than via `#[tokio::main]`, so the main thread stays
    // free for Slint. Held for the life of the process: dropping it would abort
    // every background task.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("pecu-worker")
        .build()?;

    tracing::info!(
        sdk_rev = pecu_protocol::SDK_REV,
        mock = cfg!(feature = "mock"),
        "starting"
    );

    // The chain this home was last used for. Testnet when there is no answer —
    // never launched, or a file this build does not recognise — because putting
    // a half-finished wallet on mainnet would be a choice made for the user
    // rather than by them.
    //
    // Read **before** the nodes, because the endpoints are a function of it:
    // the shipped list is per chain now, so a wallet on VRSC is no longer
    // offered a testnet endpoint it would mark `WrongNetwork` the moment it
    // answered.
    let network = Paths::remembered(&home).unwrap_or(Network::Testnet);
    let nodes = pecu_core::shipped_nodes(&network, cfg!(feature = "mock"));
    let paths = Paths::new(home.clone(), &network, cfg!(feature = "mock"));

    let (dispatcher, events) = pecu_core::start(
        runtime.handle(),
        Config {
            nodes,
            network,
            mock: cfg!(feature = "mock"),
            home,
        },
    );

    let ui = AppWindow::new()?;
    ui.global::<AppInfo>()
        .set_sdk_rev(pecu_protocol::SDK_REV[..8].into());
    ui.global::<AppInfo>()
        .set_search_shortcut(pecu_ui::search_shortcut().into());
    ui.global::<NetworkState>()
        .set_mock_mode(cfg!(feature = "mock"));

    ui.global::<WalletState>()
        .set_vault_path(paths.vault().display().to_string().into());
    ui.global::<AppInfo>()
        .set_log_path(log_dir().display().to_string().into());

    pecu_ui::chart::install(&ui);
    pecu_ui::spark::install(&ui);
    pecu_ui::toast::install(&ui);
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

    // English, on purpose, and not because it is the default.
    //
    // Slint picks a bundled translation from the **system locale** when the
    // first component is built. There is a `de` catalogue in the tree — eight
    // navigation labels, a fixture that exists so `tests/translation.rs` can
    // prove the machinery works — so without this line a wallet started on a
    // German system comes up with a German rail and an English everything else.
    //
    // The language is going to be a setting. Until it is one, and until there
    // is a catalogue somebody has actually translated, the honest behaviour is
    // one language rather than a random half of two. `select_bundled_translation`
    // has to run after the first component exists, which is why it is here and
    // not at the top of `main`.
    if let Err(error) = slint::select_bundled_translation("en") {
        tracing::warn!(?error, "could not pin the interface language to English");
    }

    ui.show()?;
    ui.invoke_focus_shortcuts();

    let _runtime = runtime;
    slint::run_event_loop()?;
    Ok(())
}

/// Say so, and stop.
///
/// A window rather than a line on stderr, because an application launched from
/// the Finder has no stderr and a silent exit looks exactly like a crash — see
/// `AlreadyRunning`.
fn already_running() -> Result<(), Box<dyn std::error::Error>> {
    tracing::warn!("a second instance was started; refusing");
    let window = pecu_ui::AlreadyRunning::new()?;
    {
        let weak = window.as_weak();
        window.on_dismissed(move || {
            if let Some(window) = weak.upgrade() {
                let _ = window.hide();
            }
            let _ = slint::quit_event_loop();
        });
    }
    window.show()?;
    slint::run_event_loop()?;
    Ok(())
}

/// Where an application keeps data on each of the three desktops.
///
/// A value rather than a `cfg`, and that is the point: with the platform
/// gated at the `let`, two of these three branches are never compiled on any
/// machine that builds this wallet, and the first time they are read by a
/// compiler is on somebody else's computer. As data, all three type-check
/// everywhere and all three are covered by tests here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Home {
    /// `~/Library/Application Support/com.pecu.wallet`
    MacOs,
    /// `%APPDATA%\\Pecu`
    Windows,
    /// `$XDG_DATA_HOME/pecu`, falling back to `~/.local/share/pecu`
    Xdg,
}

impl Home {
    /// The one this build is for.
    const CURRENT: Self = if cfg!(target_os = "macos") {
        Self::MacOs
    } else if cfg!(target_os = "windows") {
        Self::Windows
    } else {
        Self::Xdg
    };

    /// Resolve against an environment, or `None` if it does not say enough.
    ///
    /// The environment is a parameter so the tests can hand it one that is not
    /// this machine's — which is the only way the Windows and Linux answers get
    /// checked from here at all.
    fn resolve(self, env: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
        match self {
            // The reverse-DNS name is macOS's convention and stays here. On the
            // other two it would be a macOS habit worn in the wrong place.
            Self::MacOs => env("HOME")
                .map(|home| PathBuf::from(home).join("Library/Application Support/com.pecu.wallet")),

            // `APPDATA` is the roaming profile, which is what an installed
            // application is expected to write to. `HOME` is deliberately not
            // consulted: on Windows it is set by unix-shell ports and points
            // somewhere no other application looks.
            Self::Windows => env("APPDATA")
                .map(|roaming| PathBuf::from(roaming).join("Pecu"))
                .or_else(|| {
                    env("USERPROFILE")
                        .map(|profile| PathBuf::from(profile).join("AppData/Roaming/Pecu"))
                }),

            // Data rather than config: what lives under here is a vault and two
            // databases, not preferences somebody might reasonably edit or
            // check into a dotfiles repository.
            //
            // A relative `XDG_DATA_HOME` is ignored. The specification requires
            // it to be absolute, and honouring a relative one would put a vault
            // wherever the process happened to be started from.
            Self::Xdg => env("XDG_DATA_HOME")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .or_else(|| env("HOME").map(|home| PathBuf::from(home).join(".local/share")))
                .map(|data| data.join("pecu")),
        }
    }
}

/// The application home, holding one directory per chain.
///
/// Which chain's directory is opened inside it is [`Paths`]' decision, and the
/// choice is remembered at the top of this one — see [`pecu_core::paths`] for
/// why that is the one thing not kept per chain.
///
/// `PECU_HOME` wins on every platform. It is what lets one machine hold a
/// second wallet, and it is what the tests drive.
fn home_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("PECU_HOME") {
        return PathBuf::from(home);
    }

    // Wrapped rather than passed by name: `var_os` is generic over its key, so
    // naming it directly gives the compiler one fixed lifetime where `resolve`
    // asks for any.
    Home::CURRENT.resolve(|key| std::env::var_os(key)).unwrap_or_else(|| {
        // Nothing to derive a home from. The wallet still starts — every open
        // below this fails on its own terms and says so, which is a better
        // failure than refusing to launch — but it says so loudly, because a
        // vault written relative to the working directory is one that vanishes
        // the next time the application starts from somewhere else.
        tracing::error!(
            "no home directory could be determined from the environment; falling back to \
             the working directory. Set PECU_HOME to choose one."
        );
        PathBuf::from(".")
    })
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
    wire_history(ui, dispatcher.clone());
    wire_search(ui, dispatcher.clone());
    wire_links(ui);
    wire_markets(ui, dispatcher.clone());
    wire_convert(ui, dispatcher.clone());
    wire_wallet(ui, dispatcher.clone());
    wire_backup(ui, &dispatcher);
    wire_send(ui, &dispatcher);
    wire_identity(ui, &dispatcher);
    wire_currency(ui, &dispatcher);
    wire_reserve_picker(ui, &dispatcher);
    wire_launch(ui, &dispatcher);
    wire_settings(ui, &dispatcher);
    wire_light_server(ui, &dispatcher);
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
                wallet.set_problem(pecu_ui::Note::default());
                wallet.set_busy(true);
            }
            dispatcher.send(Command::CreateWallet {
                name: "Pecu".to_string(),
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
                wallet.set_problem(pecu_ui::Note::default());
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
                wallet.set_problem(pecu_ui::Note::default());
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
            seed.set_problem(pecu_ui::Note::default());

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
                pecu_ui::seed::close(&ui);
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
            seed.set_problem(pecu_ui::Note::default());

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
        actions.on_set_spending(move |on, typed| {
            // The typed word is passed straight through. Checking it here would
            // put the guard in the layer that is easiest to bypass — and the
            // word differs per chain, so there is not even a literal to check
            // against.
            dispatcher.send(Command::SetAllowSpending {
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
/// The interface says which balance pays with a bool; the core wants the name.
///
/// One place rather than two, so the two callbacks cannot disagree about which
/// way round it is.
const fn pool(shielded: bool) -> pecu_protocol::Pool {
    if shielded {
        pecu_protocol::Pool::Shielded
    } else {
        pecu_protocol::Pool::Transparent
    }
}

fn wire_send(ui: &AppWindow, dispatcher: &Dispatcher) {
    let actions = ui.global::<Actions>();

    {
        let dispatcher = dispatcher.clone();
        actions.on_validate_draft(move |to, amount, from_shielded| {
            dispatcher.send(Command::ValidateDraft(SendDraft {
                from_label: String::new(),
                to: to.to_string(),
                amount: amount.to_string(),
                from_pool: pool(from_shielded),
            }));
        });
    }

    {
        let dispatcher = dispatcher.clone();
        let weak = ui.as_weak();
        actions.on_prepare_send(move |to, amount, from_shielded| {
            if let Some(ui) = weak.upgrade() {
                let send = ui.global::<SendState>();
                send.set_problem(pecu_ui::Note::default());
            }
            dispatcher.send(Command::PrepareSend(SendDraft {
                from_label: String::new(),
                to: to.to_string(),
                amount: amount.to_string(),
                from_pool: pool(from_shielded),
            }));
        });
    }

    {
        let dispatcher = dispatcher.clone();
        let weak = ui.as_weak();
        actions.on_confirm_send(move |ticket| {
            if let Some(ui) = weak.upgrade() {
                ui.global::<SendState>().set_problem(pecu_ui::Note::default());
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
/// The VerusIDs screen.
///
/// Nothing here can be handed a secret: reading an identity needs no key, and
/// the write operations that will need one are not built yet. When they are,
/// they belong beside `wire_wallet`, not here.
/// The currency reads.
///
/// Its own function rather than more of `wire_identity`, even though a currency
/// is an identity: these are two screens, and one wiring function that grew
/// past a hundred lines is one nobody reads to the end. Nothing here signs
/// anything — defining a currency is a later step and is not wired yet.
/// Every reserve address already in the draft, apart from the row being chosen
/// for.
///
/// Read off the interface rather than out of the core's copy of the draft: that
/// copy may be a debounce behind what is on screen, and a picker built from it
/// would offer a currency added to the basket half a second ago — which the same
/// core then refuses as a duplicate.
fn reserves_taken(ui: &AppWindow, except: i32) -> Vec<String> {
    ui.global::<pecu_ui::CurrencyState>()
        .get_reserve_rows()
        .iter()
        .enumerate()
        .filter(|(index, _)| i32::try_from(*index) != Ok(except))
        .map(|(_, row)| row.currency.to_string())
        .filter(|address| !address.trim().is_empty())
        .collect()
}

/// Open the picker for one reserve row, on the start of the list.
fn open_reserve_picker(ui: &AppWindow, dispatcher: &Dispatcher, row: i32) {
    let state = ui.global::<pecu_ui::CurrencyState>();
    state.set_picking_reserve(row);
    state.set_choices_query(SharedString::new());
    dispatcher.send(Command::SearchCurrencies {
        query: String::new(),
        exclude: reserves_taken(ui, row),
    });
}

/// Choosing a reserve out of the chain's own currency list.
///
/// Its own function because `wire_currency` was already long, and because this
/// is the half of the reserve rows that talks to the chain rather than to the
/// draft — the same split `wire_send` and `wire_launch` follow.
fn wire_reserve_picker(ui: &AppWindow, dispatcher: &Dispatcher) {
    let actions = ui.global::<Actions>();

    // Choosing a reserve out of what the chain has.
    //
    // The exclusions are read here rather than in the core: the draft the core
    // last saw may be a debounce behind what is on screen, and offering a
    // currency that was added to the basket half a second ago would be offering
    // a duplicate the same core then refuses.
    {
        let dispatcher = dispatcher.clone();
        let weak = ui.as_weak();
        actions.on_search_currencies(move |query| {
            if let Some(ui) = weak.upgrade() {
                let picking = ui
                    .global::<pecu_ui::CurrencyState>()
                    .get_picking_reserve();
                dispatcher.send(Command::SearchCurrencies {
                    query: query.to_string(),
                    exclude: reserves_taken(&ui, picking),
                });
            }
        });
    }

    {
        let dispatcher = dispatcher.clone();
        let weak = ui.as_weak();
        actions.on_choose_reserve(move |index, name, address| {
            if let Some(ui) = weak.upgrade() {
                let state = ui.global::<pecu_ui::CurrencyState>();
                let mut rows: Vec<pecu_ui::ReserveEntry> =
                    state.get_reserve_rows().iter().collect();
                if let Ok(index) = usize::try_from(index) {
                    if let Some(row) = rows.get_mut(index) {
                        row.currency = address;
                        row.name = name;
                    }
                }
                state.set_reserve_rows(ModelRc::from(Rc::new(VecModel::from(rows))));
                dispatcher.send(Command::ValidateCurrency(draft_of(&ui)));
            }
        });
    }
}

fn wire_currency(ui: &AppWindow, dispatcher: &Dispatcher) {
    let actions = ui.global::<Actions>();

    {
        // Marks the walk as running here rather than waiting for the core to
        // say so: the request goes out immediately and the button has to stop
        // being pressable in the same frame. `apply_currencies` clears it.
        let dispatcher = dispatcher.clone();
        let weak = ui.as_weak();
        actions.on_refresh_currencies(move || {
            if let Some(ui) = weak.upgrade() {
                ui.global::<pecu_ui::CurrencyState>().set_busy(true);
            }
            dispatcher.send(Command::RefreshCurrencies);
        });
    }

    {
        let dispatcher = dispatcher.clone();
        actions.on_open_currency(move |address| {
            dispatcher.send(Command::OpenCurrency(address.to_string()));
        });
    }

    {
        let dispatcher = dispatcher.clone();
        let weak = ui.as_weak();
        actions.on_check_currency(move || {
            if let Some(ui) = weak.upgrade() {
                dispatcher.send(Command::ValidateCurrency(draft_of(&ui)));
            }
        });
    }

    // Adding and removing a row is a length change, which Slint cannot do from
    // a binding. Each of these rebuilds the model and re-checks, so the picture
    // never lags the form by an edit.
    {
        let dispatcher = dispatcher.clone();
        let weak = ui.as_weak();
        actions.on_add_reserve(move || {
            if let Some(ui) = weak.upgrade() {
                let state = ui.global::<pecu_ui::CurrencyState>();
                let mut rows: Vec<pecu_ui::ReserveEntry> =
                    state.get_reserve_rows().iter().collect();
                rows.push(pecu_ui::ReserveEntry::default());
                let added = i32::try_from(rows.len().saturating_sub(1)).unwrap_or(0);
                state.set_reserve_rows(ModelRc::from(Rc::new(VecModel::from(rows))));
                dispatcher.send(Command::ValidateCurrency(draft_of(&ui)));
                // Straight into the picker. "Add a reserve" means adding a
                // currency, and an empty row with a weight field beside it is
                // the step before that rather than the thing asked for.
                open_reserve_picker(&ui, &dispatcher, added);
            }
        });
    }

    {
        let dispatcher = dispatcher.clone();
        let weak = ui.as_weak();
        actions.on_remove_reserve(move |index| {
            if let Some(ui) = weak.upgrade() {
                let state = ui.global::<pecu_ui::CurrencyState>();
                let mut rows: Vec<pecu_ui::ReserveEntry> =
                    state.get_reserve_rows().iter().collect();
                if let Ok(index) = usize::try_from(index) {
                    if index < rows.len() {
                        rows.remove(index);
                    }
                }
                state.set_reserve_rows(ModelRc::from(Rc::new(VecModel::from(rows))));
                dispatcher.send(Command::ValidateCurrency(draft_of(&ui)));
            }
        });
    }

    {
        let dispatcher = dispatcher.clone();
        let weak = ui.as_weak();
        actions.on_add_preallocation(move || {
            if let Some(ui) = weak.upgrade() {
                let state = ui.global::<pecu_ui::CurrencyState>();
                let mut rows: Vec<pecu_ui::PreallocEntry> =
                    state.get_supply_rows().iter().collect();
                rows.push(pecu_ui::PreallocEntry::default());
                state.set_supply_rows(ModelRc::from(Rc::new(VecModel::from(rows))));
                dispatcher.send(Command::ValidateCurrency(draft_of(&ui)));
            }
        });
    }

    {
        let dispatcher = dispatcher.clone();
        let weak = ui.as_weak();
        actions.on_remove_preallocation(move |index| {
            if let Some(ui) = weak.upgrade() {
                let state = ui.global::<pecu_ui::CurrencyState>();
                let mut rows: Vec<pecu_ui::PreallocEntry> =
                    state.get_supply_rows().iter().collect();
                if let Ok(index) = usize::try_from(index) {
                    if index < rows.len() {
                        rows.remove(index);
                    }
                }
                state.set_supply_rows(ModelRc::from(Rc::new(VecModel::from(rows))));
                dispatcher.send(Command::ValidateCurrency(draft_of(&ui)));
            }
        });
    }
}

/// Building, sending and abandoning a launch.
///
/// Its own function because it is the half of the currency screen that signs
/// something — the same split `wire_send` and `wire_identity` follow, so the
/// surface worth reviewing closely stays short.
fn wire_launch(ui: &AppWindow, dispatcher: &Dispatcher) {
    let actions = ui.global::<Actions>();

    {
        let dispatcher = dispatcher.clone();
        let weak = ui.as_weak();
        actions.on_prepare_launch(move || {
            if let Some(ui) = weak.upgrade() {
                dispatcher.send(Command::PrepareLaunch(draft_of(&ui)));
            }
        });
    }

    {
        let dispatcher = dispatcher.clone();
        let weak = ui.as_weak();
        actions.on_confirm_launch(move |ticket| {
            if let Some(ui) = weak.upgrade() {
                // Marked here rather than waiting for the core to echo it: the
                // button must stop being pressable in the same frame, or a
                // second press sends a ticket the core has already taken.
                ui.global::<pecu_ui::CurrencyState>()
                    .set_launch_busy(true);
            }
            dispatcher.send(Command::ConfirmLaunch {
                ticket: u64::try_from(ticket).unwrap_or(0),
            });
        });
    }

    {
        let dispatcher = dispatcher.clone();
        actions.on_check_currency_name(move |name| {
            dispatcher.send(Command::CheckName(name.to_string()));
        });
    }

    {
        let dispatcher = dispatcher.clone();
        let handle = ui.as_weak();
        actions.on_start_currency_from_new_name(move || {
            let Some(ui) = handle.upgrade() else {
                return;
            };
            let state = ui.global::<pecu_ui::CurrencyState>();
            dispatcher.send(Command::StartCurrencyFromNewName {
                revocation_authority: state.get_new_revocation().trim().to_string(),
                recovery_authority: state.get_new_recovery().trim().to_string(),
                draft: draft_of(&ui),
            });
        });
    }

    {
        let dispatcher = dispatcher.clone();
        actions.on_resume_launch(move || dispatcher.send(Command::ResumeLaunch));
    }

    {
        let dispatcher = dispatcher.clone();
        actions.on_abandon_launch(move || dispatcher.send(Command::AbandonLaunch));
    }

    {
        let dispatcher = dispatcher.clone();
        actions.on_cancel_launch(move |ticket| {
            dispatcher.send(Command::CancelLaunch {
                ticket: u64::try_from(ticket).unwrap_or(0),
            });
        });
    }
}

/// The draft as the interface currently holds it.
///
/// Read off `CurrencyState` rather than passed through a callback signature:
/// eight parameters, two of them lists, is a signature nobody can change
/// without changing five call sites — and every edit sends the whole draft
/// anyway, because a basket's weights are only wrong together.
fn draft_of(ui: &AppWindow) -> pecu_protocol::CurrencyDraft {
    let state = ui.global::<pecu_ui::CurrencyState>();
    pecu_protocol::CurrencyDraft {
        kind: state.get_kind().to_string(),
        // Exactly one of the two, decided by which way in is showing rather
        // than by which field happens to be non-empty. A name left behind by a
        // change of mind must not travel with a draft that names an identity —
        // core refuses a draft carrying both, and it is right to.
        identity: if state.get_claiming() {
            String::new()
        } else {
            state.get_identity().to_string()
        },
        new_name: if state.get_claiming() {
            state.get_new_name().trim().to_string()
        } else {
            String::new()
        },
        mintable: state.get_mintable(),
        start_delay: state.get_start_delay().to_string(),
        reserves: state
            .get_reserve_rows()
            .iter()
            .map(|row| pecu_protocol::ReserveDraft {
                currency: row.currency.to_string(),
                name: row.name.to_string(),
                weight: row.weight.to_string(),
            })
            .collect(),
        preallocations: state
            .get_supply_rows()
            .iter()
            .map(|row| pecu_protocol::PreallocationDraft {
                recipient: row.recipient.to_string(),
                amount: row.amount.to_string(),
            })
            .collect(),
    }
}

fn wire_identity(ui: &AppWindow, dispatcher: &Dispatcher) {
    let actions = ui.global::<Actions>();

    {
        let dispatcher = dispatcher.clone();
        actions.on_refresh_identities(move || {
            dispatcher.send(Command::RefreshIdentities);
        });
    }

    {
        let dispatcher = dispatcher.clone();
        let weak = ui.as_weak();
        actions.on_look_up_identity(move |typed| {
            if let Some(ui) = weak.upgrade() {
                ui.global::<pecu_ui::IdentityState>()
                    .set_lookup_problem(pecu_ui::Note::default());
            }
            dispatcher.send(Command::LookUpIdentity(typed.to_string()));
        });
    }

    {
        let dispatcher = dispatcher.clone();
        actions.on_open_identity(move |address| {
            dispatcher.send(Command::OpenIdentity(address.to_string()));
        });
    }

    {
        let weak = ui.as_weak();
        // Closing is the UI's own business: no request goes out, and waiting
        // for the core to echo it back would put a round trip between the click
        // and the sheet going away.
        actions.on_close_identity(move || {
            if let Some(ui) = weak.upgrade() {
                let state = ui.global::<pecu_ui::IdentityState>();
                state.set_address(slint::SharedString::new());
                state.set_derived_key(slint::SharedString::new());
                state.set_key_draft(slint::SharedString::new());
                // The authority form shuts with the sheet, and empties.
                //
                // Here rather than on the sheet's `init`, even though `init`
                // would cover every route in one place: every close does go
                // through this callback, and a reset on `init` would also fire
                // after a fixture had deliberately opened the form — which
                // would leave the reference image photographing a shut form
                // and nothing covering the fields.
                state.set_changing_authorities(false);
                state.set_new_revocation(slint::SharedString::new());
                state.set_new_recovery(slint::SharedString::new());
            }
        });
    }

    {
        let dispatcher = dispatcher.clone();
        actions.on_derive_content_key(move |uri| {
            dispatcher.send(Command::DeriveContentKey(uri.to_string()));
        });
    }

    {
        let dispatcher = dispatcher.clone();
        actions.on_clear_lookups(move || {
            dispatcher.send(Command::ClearLookups);
        });
    }

    {
        let dispatcher = dispatcher.clone();
        actions.on_unwatch_identity(move |address| {
            dispatcher.send(Command::UnwatchIdentity(address.to_string()));
        });
    }

    {
        let dispatcher = dispatcher.clone();
        actions.on_check_name(move |name| {
            dispatcher.send(Command::CheckName(name.to_string()));
        });
    }

    {
        let dispatcher = dispatcher.clone();
        actions.on_start_registration(move |name, revocation, recovery| {
            dispatcher.send(Command::StartRegistration {
                name: name.to_string(),
                revocation_authority: revocation.to_string(),
                recovery_authority: recovery.to_string(),
            });
        });
    }

    {
        let dispatcher = dispatcher.clone();
        actions.on_finish_registration(move || {
            dispatcher.send(Command::FinishRegistration);
        });
    }

    {
        let dispatcher = dispatcher.clone();
        actions.on_abandon_registration(move || {
            dispatcher.send(Command::AbandonRegistration);
        });
    }

    wire_identity_writes(ui, dispatcher);
}

/// The VerusID operations that build a transaction.
///
/// Split from the reads for the same reason `wire_wallet` is split from
/// `wire_shell`: the surface worth reading closely should be short. Nothing here
/// carries a secret either — the vault is reached by label — but every one of
/// these ends in something signed, and three of them cannot be undone.
fn wire_identity_writes(ui: &AppWindow, dispatcher: &Dispatcher) {
    let actions = ui.global::<Actions>();

    {
        let dispatcher = dispatcher.clone();
        actions.on_set_identity_authorities(move |address, revocation, recovery| {
            dispatcher.send(Command::SetIdentityAuthorities {
                address: address.to_string(),
                revocation: revocation.to_string(),
                recovery: recovery.to_string(),
            });
        });
    }

    {
        let dispatcher = dispatcher.clone();
        actions.on_lock_identity(move |address, blocks| {
            dispatcher.send(Command::LockIdentity {
                address: address.to_string(),
                delay_blocks: u32::try_from(blocks).unwrap_or(0),
            });
        });
    }

    {
        let dispatcher = dispatcher.clone();
        actions.on_unlock_identity(move |address, blocks| {
            dispatcher.send(Command::UnlockIdentity {
                address: address.to_string(),
                extra_blocks: u32::try_from(blocks).unwrap_or(0),
            });
        });
    }

    {
        let dispatcher = dispatcher.clone();
        actions.on_confirm_identity_change(move |ticket, typed| {
            dispatcher.send(Command::ConfirmIdentityChange {
                ticket: ticket_id(ticket),
                typed: typed.to_string(),
            });
        });
    }

    {
        let dispatcher = dispatcher.clone();
        actions.on_revoke_identity(move |address| {
            dispatcher.send(Command::RevokeIdentity {
                address: address.to_string(),
            });
        });
    }

    {
        let dispatcher = dispatcher.clone();
        actions.on_recover_identity(move |address| {
            dispatcher.send(Command::RecoverIdentity {
                address: address.to_string(),
            });
        });
    }

    {
        let dispatcher = dispatcher.clone();
        let weak = ui.as_weak();
        actions.on_cancel_identity_change(move |ticket| {
            if let Some(ui) = weak.upgrade() {
                ui.global::<pecu_ui::IdentityState>()
                    .set_change_ticket(0);
            }
            dispatcher.send(Command::CancelIdentityChange {
                ticket: ticket_id(ticket),
            });
        });
    }
}

/// The command palette.
///
/// Its own function rather than three more blocks in [`wire_shell`], which was
/// already at the length where a reader stops holding the whole thing in their
/// head — and this is the one part of the shell that talks to the core on every
/// keystroke, so it is worth being able to find.
/// The history filter.
///
/// One line of wiring, and it is in the core for a reason worth keeping in view:
/// a `ListView` cannot skip rows, and paging a filtered list is a question only
/// the side holding the list can answer. See `Command::SetHistoryFilter`.
fn wire_history(ui: &AppWindow, dispatcher: Dispatcher) {
    let activity = ui.global::<pecu_ui::ActivityState>();
    activity.on_filter_changed(move |kind| {
        dispatcher.send(Command::SetHistoryFilter {
            kind: kind.to_string(),
        });
    });
}

/// Composing a conversion.
///
/// Nothing here decides anything. The three draft fields go to the core on
/// every edit and it answers with a whole quote — including whether there is
/// enough to act on, which is a question about routes and balances and not
/// about how much text is in a box.
fn wire_convert(ui: &AppWindow, dispatcher: Dispatcher) {
    let convert = ui.global::<pecu_ui::ConvertState>();

    {
        let dispatcher = dispatcher.clone();
        let weak = ui.as_weak();
        convert.on_edited(move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let state = ui.global::<pecu_ui::ConvertState>();
            dispatcher.send(Command::SetConvertDraft(pecu_protocol::ConvertDraft {
                from: state.get_from_address().to_string(),
                to: state.get_to_address().to_string(),
                pay: state.get_pay_draft().to_string(),
            }));
        });
    }

    {
        // The swap is the core's, because what happens to the amount already
        // typed is a decision about money. The two address fields are written
        // back here so the boxes turn over on the click rather than after the
        // round trip; the reply sets the names to match.
        let dispatcher = dispatcher.clone();
        let weak = ui.as_weak();
        convert.on_swap(move || {
            if let Some(ui) = weak.upgrade() {
                let state = ui.global::<pecu_ui::ConvertState>();
                let from = state.get_from_address();
                state.set_from_address(state.get_to_address());
                state.set_to_address(from);
                state.set_pay_draft(slint::SharedString::new());
            }
            dispatcher.send(Command::SwapConvertLegs);
        });
    }

    {
        // Build and sign. **Sends nothing** — the core's `PrepareConversion`
        // takes no arguments precisely so this cannot describe a conversion
        // different from the one the quote on screen is about.
        let dispatcher = dispatcher.clone();
        let weak = ui.as_weak();
        convert.on_review(move || {
            if let Some(ui) = weak.upgrade() {
                ui.global::<pecu_ui::ConvertState>()
                    .set_problem(pecu_ui::Note::default());
            }
            dispatcher.send(Command::PrepareConversion);
        });
    }

    {
        // The one press on this screen that spends. The ticket is the only
        // handle the interface has: the signed bytes never came over.
        let dispatcher = dispatcher.clone();
        let weak = ui.as_weak();
        convert.on_confirm(move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let ticket = ui.global::<pecu_ui::ConvertState>().get_ticket();
            dispatcher.send(Command::ConfirmConversion {
                ticket: u64::try_from(ticket).unwrap_or_default(),
            });
        });
    }

    {
        // Back to the form, and the core forgets the bytes. Told rather than
        // merely navigated away from: a signed conversion nobody agreed to
        // send should not sit in memory waiting for a ticket number to be
        // guessed.
        let dispatcher = dispatcher.clone();
        let weak = ui.as_weak();
        convert.on_cancel(move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let state = ui.global::<pecu_ui::ConvertState>();
            let ticket = state.get_ticket();
            state.set_problem(pecu_ui::Note::default());
            state.set_step("form".into());
            dispatcher.send(Command::CancelConversion {
                ticket: u64::try_from(ticket).unwrap_or_default(),
            });
        });
    }

    {
        // Finished with the result. The amount is cleared and the two
        // currencies are not: converting again in the same direction is the
        // likely next thing, and re-picking both to do it would be a form
        // punishing somebody for having used it.
        let weak = ui.as_weak();
        convert.on_done(move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let state = ui.global::<pecu_ui::ConvertState>();
            state.set_pay_draft(slint::SharedString::new());
            state.set_txid(slint::SharedString::new());
            state.set_problem(pecu_ui::Note::default());
            state.set_step("form".into());
            dispatcher.send(Command::SetConvertDraft(pecu_protocol::ConvertDraft {
                from: state.get_from_address().to_string(),
                to: state.get_to_address().to_string(),
                pay: String::new(),
            }));
        });
    }
}

/// Picking a row on the markets screen.
///
/// The core answers from the book it already holds — no request, so no
/// debounce and no in-flight state to track here. Selecting is the only thing
/// this screen can do: the figures on it are read, and everything that would
/// act on them lives on Convert.
fn wire_markets(ui: &AppWindow, dispatcher: Dispatcher) {
    let markets = ui.global::<pecu_ui::MarketState>();
    let weak = ui.as_weak();
    markets.on_select(move |address| {
        // Written here as well as by the reply, so the row highlights on the
        // click rather than after the round trip through the core. The reply
        // sets it again to the same value, which is what keeps a selection the
        // core refused from sticking.
        if let Some(ui) = weak.upgrade() {
            ui.global::<pecu_ui::MarketState>().set_selected(address.clone());
        }
        dispatcher.send(Command::OpenMarket(address.to_string()));
    });
}

fn wire_search(ui: &AppWindow, dispatcher: Dispatcher) {
    let search = ui.global::<pecu_ui::SearchState>();

    {
        // Every keystroke asks the core, because the interface cannot answer:
        // Slint's string type has no substring test, so "does this contain what
        // was typed" is not a question `.slint` can ask. See `SearchHitVm`.
        //
        // Not debounced. The search is a filter over two lists already in
        // memory, with no request and no I/O behind it, so a timer here would
        // add latency and buy nothing.
        let dispatcher = dispatcher.clone();
        search.on_search(move |query| {
            dispatcher.send(Command::Search {
                query: query.to_string(),
            });
        });
    }

    {
        // Picking a result goes to the screen that can show it, rather than
        // opening a sheet over the palette: a detour that lands somewhere
        // recognisable is one a person can find their way back from.
        //
        // Both arms land on a screen that is in the rail. That is the whole
        // reason the core stopped answering with identities — a result whose
        // only destination is hidden strands whoever picked it.
        let weak = ui.as_weak();
        search.on_pick(move |hit| {
            let Some(ui) = weak.upgrade() else {
                return;
            };

            if hit.kind == "address" {
                // Filled in, not sent. The form is where a payment starts and
                // the amount is still empty, so this is the same state as
                // having pasted the address — one field further along, and no
                // further.
                //
                // `step` is set as well as the text: arriving at a half-built
                // review because the last visit left one there would put the
                // recipient somewhere nobody can see it.
                let send = ui.global::<pecu_ui::SendState>();
                send.set_step("form".into());
                send.set_to_draft(hit.target.clone());
                // Through the same callback the field uses, so the note, the
                // validity flag and the label under the box are filled by the
                // one piece of code that knows how — rather than by a second
                // path that would have to agree with it forever.
                ui.global::<Actions>().invoke_validate_draft(
                    hit.target.clone(),
                    send.get_amount_draft(),
                    // Whatever the form is already set to. Picking a recipient
                    // from search must not quietly change which balance pays.
                    send.get_from_shielded(),
                );
                ui.invoke_go("send".into());
                return;
            }

            // Written here as well as dispatched, for the reason `on_select`
            // gives on the markets screen: the row highlights on the pick
            // rather than after the round trip, and the reply setting it again
            // to the same value is what keeps a selection the core refused from
            // sticking.
            ui.global::<pecu_ui::MarketState>()
                .set_selected(hit.target.clone());
            dispatcher.send(Command::OpenMarket(hit.target.to_string()));
            ui.invoke_go("markets".into());
        });
    }
}

/// Opening a link in the browser.
///
/// Its own function rather than a block inside `wire_shell`, because it is the
/// only callback in this application that starts a process and it goes nowhere
/// near the core — where to look a transaction up is a fact about this machine
/// and this chain, not about the wallet.
fn wire_links(ui: &AppWindow) {
    ui.global::<Actions>()
        .on_open_link(move |url| open_in_browser(&url));
}

/// Where shielded notes are read from.
///
/// Its own function rather than another block inside `wire_shell`, which was
/// already at the length clippy stops reading at. The shielded server is not a
/// shell concern anyway — it is a second endpoint, with its own protocol.
fn wire_light_server(ui: &AppWindow, dispatcher: &Dispatcher) {
    let dispatcher = dispatcher.clone();
    ui.global::<Actions>().on_set_light_server(move |url| {
        dispatcher.send(Command::SetLightServer(url.to_string()));
    });
}

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
        actions.on_set_requested_network(move |name| {
            dispatcher.send(Command::SetRequestedNetwork(name.to_string()));
        });
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

    {
        let dispatcher = dispatcher.clone();
        actions.on_remember_window(move |width, height| {
            // Negatives cannot reach here — the window reports its own size —
            // and a zero would be refused by the core anyway. Clamped rather
            // than unwrapped so a surprising value is dropped instead of
            // panicking the UI thread.
            dispatcher.send(Command::RememberWindow {
                width: u32::try_from(width).unwrap_or(0),
                height: u32::try_from(height).unwrap_or(0),
            });
        });
    }

    actions.on_navigate(move |screen| {
        let id = match screen.as_str() {
            "send" => ScreenId::Send,
            "receive" => ScreenId::Receive,
            "activity" => ScreenId::Activity,
            "nodes" => ScreenId::Nodes,
            "identities" => ScreenId::Identities,
            "currencies" => ScreenId::Currencies,
            "markets" => ScreenId::Markets,
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
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("pecu=info,warn"))
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

    let appender = tracing_appender::rolling::daily(&logs, "pecu.log");
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
    home_dir().join("logs")
}

#[cfg(test)]
mod tests {
    use super::{Home, writable_log_directory};
    use std::ffi::OsString;
    use std::path::PathBuf;

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

    // ── Where the wallet lives on each desktop ──────────────────────────────
    //
    // These run on whatever machine builds this, and they check all three
    // platforms — which is the reason `Home` is a value and not a `cfg`. Two
    // of these three answers are otherwise never compiled here, let alone run.

    /// An environment that is not this machine's.
    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> + use<> {
        let owned: Vec<(String, OsString)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), OsString::from(*v)))
            .collect();
        move |key| {
            owned
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
        }
    }

    #[test]
    fn macos_keeps_the_reverse_dns_name_under_application_support() {
        let home = Home::MacOs.resolve(env_of(&[("HOME", "/Users/someone")]));
        assert_eq!(
            home,
            Some(PathBuf::from(
                "/Users/someone/Library/Application Support/com.pecu.wallet"
            ))
        );
    }

    #[test]
    fn windows_uses_the_roaming_profile_and_a_plain_product_name() {
        let home = Home::Windows.resolve(env_of(&[("APPDATA", r"C:\Users\someone\AppData\Roaming")]));
        assert_eq!(
            home,
            Some(PathBuf::from(r"C:\Users\someone\AppData\Roaming").join("Pecu"))
        );
    }

    /// The failure this whole change exists for.
    ///
    /// `HOME` is not set on Windows by the system — it is set by unix-shell
    /// ports, and it points somewhere no other Windows application looks. The
    /// previous implementation consulted it and nothing else, so on a plain
    /// Windows install it fell through to the working directory and wrote a
    /// vault next to whatever the process was started from.
    #[test]
    fn windows_does_not_answer_to_home() {
        assert_eq!(Home::Windows.resolve(env_of(&[("HOME", "/home/someone")])), None);
    }

    #[test]
    fn windows_falls_back_to_the_user_profile() {
        let home = Home::Windows.resolve(env_of(&[("USERPROFILE", r"C:\Users\someone")]));
        assert_eq!(
            home,
            Some(PathBuf::from(r"C:\Users\someone").join("AppData/Roaming/Pecu"))
        );
    }

    #[test]
    fn linux_prefers_xdg_data_home() {
        let home = Home::Xdg.resolve(env_of(&[
            ("XDG_DATA_HOME", "/home/someone/.local/share"),
            ("HOME", "/home/someone"),
        ]));
        assert_eq!(home, Some(PathBuf::from("/home/someone/.local/share/pecu")));
    }

    #[test]
    fn linux_falls_back_to_dot_local_share() {
        let home = Home::Xdg.resolve(env_of(&[("HOME", "/home/someone")]));
        assert_eq!(home, Some(PathBuf::from("/home/someone/.local/share/pecu")));
    }

    /// The specification requires an absolute `XDG_DATA_HOME`, and honouring a
    /// relative one would put a vault wherever the process started.
    #[test]
    fn a_relative_xdg_data_home_is_ignored_in_favour_of_the_fallback() {
        let home = Home::Xdg.resolve(env_of(&[
            ("XDG_DATA_HOME", "relative/share"),
            ("HOME", "/home/someone"),
        ]));
        assert_eq!(home, Some(PathBuf::from("/home/someone/.local/share/pecu")));
    }

    #[test]
    fn an_environment_that_says_nothing_resolves_to_nothing() {
        for platform in [Home::MacOs, Home::Windows, Home::Xdg] {
            assert_eq!(platform.resolve(env_of(&[])), None, "{platform:?}");
        }
    }
}

/// Whether a link is one this application is willing to hand to the system.
///
/// Separate from [`open_in_browser`] so it can be tested without launching a
/// browser on whoever runs the suite.
///
/// `https://` and an unreserved URL alphabet. Deliberately narrow: everything
/// this wallet opens is a fixed host and path with a hex transaction id in it,
/// so the rule costs nothing and refuses the shapes that matter — a `cmd`
/// metacharacter on Windows, a `file://` or `javascript:` scheme anywhere.
fn is_openable(url: &str) -> bool {
    url.starts_with("https://")
        && url.len() <= 512
        && url
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-._~:/?#[]@!$'()*+,;=%".contains(c))
}

/// Hand a URL to whatever the machine opens links with.
///
/// # Why this is written out rather than pulled in
///
/// It is three lines per platform against a dependency, its transitive tree and
/// its own supply chain — for something whose whole implementation is a process
/// launch. The commands are the platform-documented ones and have not changed
/// in a decade.
///
/// # Why it validates first
///
/// Nothing here goes through a shell — `Command::new` passes arguments as
/// arguments — so there is no quoting to get wrong on macOS or Linux. Windows
/// is the exception that makes this worth doing anyway: `cmd /C start` is
/// parsed by `cmd`, and a URL containing `&` becomes a second command.
///
/// So the URL has to be one this application built: `https://`, and nothing
/// outside an unreserved URL alphabet. Both explorer links are a fixed host and
/// path with a hex txid in them, so they pass; anything a node could put in a
/// reply does not. Refusing loudly beats opening something unexpected.
fn open_in_browser(url: &str) {
    if !is_openable(url) {
        tracing::warn!(%url, "refused to open a link that this application did not build");
        return;
    }

    #[cfg(target_os = "macos")]
    let mut command = {
        let mut c = std::process::Command::new("open");
        c.arg(url);
        c
    };
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut c = std::process::Command::new("cmd");
        // The empty string is the window title `start` otherwise steals from
        // the first quoted argument, which would be the URL.
        c.args(["/C", "start", "", url]);
        c
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };

    // Spawned and not waited on: a browser that is starting up takes seconds,
    // and this is called from a UI callback.
    match command.spawn() {
        Ok(_) => tracing::info!(%url, "opened a link"),
        Err(error) => tracing::warn!(%url, %error, "could not open a link"),
    }
}

#[cfg(test)]
mod link_tests {
    use super::is_openable;

    /// The two links this application actually builds.
    #[test]
    fn the_explorer_links_this_wallet_builds_are_allowed() {
        let txid = "68320bb5eb723ca3ab3f92d26133b4309d03c59e9ce3e93dba85d68379e98883";
        assert!(is_openable(&format!(
            "https://markets.chainvue.io/testnet/tx/{txid}/"
        )));
        assert!(is_openable(&format!("https://explorer.verus.io/tx/{txid}")));
    }

    /// Anything that is not an `https` URL is refused, whatever it claims to be.
    ///
    /// `file://` and `javascript:` are the two that turn "open a link" into
    /// something else entirely, and neither can ever be something this wallet
    /// built.
    #[test]
    fn only_https_is_opened() {
        assert!(!is_openable("http://example.com"));
        assert!(!is_openable("file:///etc/passwd"));
        assert!(!is_openable("javascript:alert(1)"));
        assert!(!is_openable(""));
        assert!(!is_openable("markets.chainvue.io/tx/abc"));
    }

    /// The Windows case, which is the reason this check exists at all.
    ///
    /// `cmd /C start` parses its argument, so a URL carrying `&` or a quote
    /// would become a second command. Nothing here goes through a shell on the
    /// other two platforms, and this makes the third one boring as well.
    #[test]
    fn a_link_carrying_shell_punctuation_is_refused() {
        assert!(!is_openable("https://example.com/a&calc.exe"));
        assert!(!is_openable("https://example.com/\"a\""));
        assert!(!is_openable("https://example.com/a|b"));
        assert!(!is_openable("https://example.com/a b"));
        assert!(!is_openable("https://example.com/a^b"));
        assert!(!is_openable("https://example.com/a>b"));
    }

    /// And a length nobody meant.
    #[test]
    fn an_absurd_link_is_refused() {
        let long = format!("https://example.com/{}", "a".repeat(600));
        assert!(!is_openable(&long));
    }
}
