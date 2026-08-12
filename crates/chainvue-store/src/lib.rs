//! What the wallet remembers between runs.
//!
//! # Two databases, because "safe to delete" must be unambiguous
//!
//! **`wallet.sqlite`** holds things that cannot be reconstructed: what the user
//! chose. Settings, and later the node list and the address book. It runs
//! `synchronous = FULL`, and a `user_version` this build does not recognise is
//! a **hard error** — guessing at a schema from the future is how a setting
//! quietly turns into something else.
//!
//! **`cache.sqlite`** holds only what a node can be asked for again. Balances,
//! history, currency names. It runs `synchronous = NORMAL`, and a
//! `user_version` it does not recognise is simply **deleted and rebuilt**,
//! because the worst case is one slow refresh.
//!
//! Keeping them apart is what makes that asymmetry safe to act on. In one file
//! the strict rule would have to win everywhere, and "delete the cache" would
//! mean "delete the settings too".
//!
//! # What is deliberately not in here
//!
//! No key material, no recovery phrase, no passphrase, and no dependency that
//! could carry one — this crate cannot name `PrivateKey` or `Vault`. The vault
//! file is the only thing that holds secrets, and it is not SQLite.
//!
//! Also not here yet: the pending-broadcast ledger. It works, it is the one
//! table where a lost row costs money, and moving it would be risk without a
//! reason. It is a JSON file in `chainvue-core` and its shape is already a row.
//!
//! # Why the connection is used from the actor rather than a pool
//!
//! These are local files and the writes are tiny — a settings upsert, a
//! snapshot of a few hundred rows in one transaction. Both are well under a
//! millisecond, which is not worth a thread hop. If a snapshot ever grows
//! enough to be felt, it moves behind [`crate::Store`] on its own thread and
//! nothing above changes.

mod migrate;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chainvue_protocol::{HistoryRowVm, PortfolioVm};
use rusqlite::Connection;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("cannot open {}", path.display())]
    Open {
        path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },

    /// A durable database written by a newer build. Refused rather than
    /// guessed at — see the module docs.
    #[error("{} was written by a newer version of ChainVue (schema {found}, this build reads {expected})", path.display())]
    FromTheFuture {
        path: PathBuf,
        found: i64,
        expected: i64,
    },

    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

/// Everything the wallet remembers, for one network.
pub struct Store {
    wallet: Connection,
    cache: Connection,
}

/// A dashboard as it looked when the wallet last ran.
pub struct Snapshot {
    pub portfolio: PortfolioVm,
    pub history: Vec<HistoryRowVm>,
    /// Unix seconds. The caller decides what counts as too old to show.
    pub saved_at: i64,
}

impl Store {
    /// Open, or create, both databases in `dir`.
    ///
    /// The directory is per network, so testnet figures can never render as
    /// mainnet — a mistake no amount of care further up could undo.
    pub fn open(dir: &Path) -> Result<Self, StoreError> {
        std::fs::create_dir_all(dir).ok();

        let wallet = open_at(&dir.join("wallet.sqlite"), "FULL")?;
        migrate::wallet(&wallet, &dir.join("wallet.sqlite"))?;

        // A cache that cannot be opened is not a reason to fail: it holds
        // nothing that cannot be asked for again. Start without one.
        let cache_path = dir.join("cache.sqlite");
        let cache = match open_at(&cache_path, "NORMAL").and_then(|c| {
            migrate::cache(&c)?;
            Ok(c)
        }) {
            Ok(cache) => cache,
            Err(error) => {
                tracing::warn!(%error, "the cache could not be opened; rebuilding it");
                let _ = std::fs::remove_file(&cache_path);
                let cache = open_at(&cache_path, "NORMAL")?;
                migrate::cache(&cache)?;
                cache
            }
        };

        Ok(Self { wallet, cache })
    }

    // ── Settings: durable ───────────────────────────────────────────────────

    pub fn setting(&self, key: &str) -> Option<String> {
        self.wallet
            .query_row("SELECT value FROM setting WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .ok()
    }

    /// A setting failing to save is worth a log line and nothing more. It is a
    /// preference, and losing one must not take an operation down with it.
    pub fn set_setting(&self, key: &str, value: &str) {
        if let Err(error) = self.wallet.execute(
            "INSERT INTO setting (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            [key, value],
        ) {
            tracing::warn!(%error, key, "a setting could not be saved");
        }
    }

    // ── Cache: throw away freely ────────────────────────────────────────────

    /// The chain's own currency, learned once and true forever for that chain.
    pub fn native_currency(&self) -> Option<String> {
        self.cache
            .query_row(
                "SELECT value FROM kv WHERE key = 'native_currency'",
                [],
                |row| row.get(0),
            )
            .ok()
    }

    pub fn remember_native_currency(&self, i_address: &str) {
        self.cache_put("native_currency", i_address);
    }

    /// Every currency name seen so far, keyed by i-address.
    ///
    /// Worth caching harder than anything else here: `currency_names` costs one
    /// request **per currency**, and a name is fixed when the currency is
    /// registered and can never change. This cache never needs invalidating.
    pub fn currency_names(&self) -> BTreeMap<String, String> {
        let mut names = BTreeMap::new();
        let Ok(mut statement) = self
            .cache
            .prepare("SELECT currency, name FROM currency_name")
        else {
            return names;
        };
        let Ok(rows) = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        }) else {
            return names;
        };
        for row in rows.flatten() {
            names.insert(row.0, row.1);
        }
        names
    }

    pub fn remember_currency_names(&self, names: &BTreeMap<String, String>) {
        for (currency, name) in names {
            if let Err(error) = self.cache.execute(
                "INSERT INTO currency_name (currency, name) VALUES (?1, ?2)
                 ON CONFLICT (currency) DO UPDATE SET name = excluded.name",
                [currency.as_str(), name.as_str()],
            ) {
                tracing::warn!(%error, currency, "a currency name could not be cached");
            }
        }
    }

    /// The last dashboard, so a cold start shows figures instead of nothing.
    ///
    /// Whatever comes back is **out of date by definition** and the caller must
    /// mark it as such. It is the difference between "here is what you had, we
    /// are checking" and a blank screen for the two seconds a node takes.
    pub fn snapshot(&self) -> Option<Snapshot> {
        let (portfolio, history, saved_at): (String, String, i64) = self
            .cache
            .query_row(
                "SELECT portfolio, history, saved_at FROM snapshot WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .ok()?;

        Some(Snapshot {
            portfolio: serde_json::from_str(&portfolio).ok()?,
            history: serde_json::from_str(&history).ok()?,
            saved_at,
        })
    }

    /// Keep the current dashboard for the next cold start.
    ///
    /// # Why JSON rather than columns
    ///
    /// This is a *derived, deletable* cache of finished view models, read back
    /// whole and never queried by field. Columns mirroring `PortfolioVm` would
    /// be a second copy of a shape that already exists, kept in step by hand.
    ///
    /// The moment that stops being true is paging: asking for "history below
    /// height N" needs rows, not a blob. That is a schema migration when it
    /// happens, on a database whose whole contract is that it may be thrown
    /// away.
    pub fn save_snapshot(&self, portfolio: &PortfolioVm, history: &[HistoryRowVm], now: i64) {
        let (Ok(portfolio), Ok(history)) = (
            serde_json::to_string(portfolio),
            serde_json::to_string(history),
        ) else {
            return;
        };

        if let Err(error) = self.cache.execute(
            "INSERT INTO snapshot (id, portfolio, history, saved_at) VALUES (1, ?1, ?2, ?3)
             ON CONFLICT (id) DO UPDATE SET
                 portfolio = excluded.portfolio,
                 history   = excluded.history,
                 saved_at  = excluded.saved_at",
            rusqlite::params![portfolio, history, now],
        ) {
            tracing::warn!(%error, "the dashboard snapshot could not be cached");
        }
    }

    fn cache_put(&self, key: &str, value: &str) {
        if let Err(error) = self.cache.execute(
            "INSERT INTO kv (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            [key, value],
        ) {
            tracing::warn!(%error, key, "a cache entry could not be written");
        }
    }
}

fn open_at(path: &Path, synchronous: &str) -> Result<Connection, StoreError> {
    let connection = Connection::open(path).map_err(|source| StoreError::Open {
        path: path.to_path_buf(),
        source,
    })?;

    // WAL so a reader is never blocked by a writer. `foreign_keys` because
    // SQLite defaults it OFF and a constraint nobody enforces is decoration.
    connection.pragma_update(None, "journal_mode", "WAL").ok();
    connection
        .pragma_update(None, "synchronous", synchronous)
        .ok();
    connection.pragma_update(None, "foreign_keys", "ON").ok();

    restrict(path);
    Ok(connection)
}

/// Owner-only. It says what you own and who you paid.
fn restrict(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(dir: &tempfile::TempDir) -> Store {
        Store::open(dir.path()).expect("open")
    }

    #[test]
    fn a_setting_survives_a_restart() {
        let dir = tempfile::tempdir().expect("tempdir");

        store(&dir).set_setting("auto_lock_minutes", "15");
        assert_eq!(
            store(&dir).setting("auto_lock_minutes").as_deref(),
            Some("15"),
        );

        // And a later write replaces it rather than accumulating rows.
        store(&dir).set_setting("auto_lock_minutes", "60");
        assert_eq!(
            store(&dir).setting("auto_lock_minutes").as_deref(),
            Some("60"),
        );
        assert_eq!(store(&dir).setting("never_set"), None);
    }

    #[test]
    fn currency_names_accumulate_and_survive() {
        let dir = tempfile::tempdir().expect("tempdir");

        let mut first = BTreeMap::new();
        first.insert(
            "iM2f93TPaBpFRpCydsVQfEo7zUKK2xhwfL".to_string(),
            "mambo".to_string(),
        );
        store(&dir).remember_currency_names(&first);

        let mut second = BTreeMap::new();
        second.insert(
            "iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq".to_string(),
            "VRSCTEST".to_string(),
        );
        store(&dir).remember_currency_names(&second);

        let names = store(&dir).currency_names();
        assert_eq!(names.len(), 2, "{names:?}");
        assert_eq!(
            names
                .get("iM2f93TPaBpFRpCydsVQfEo7zUKK2xhwfL")
                .map(String::as_str),
            Some("mambo"),
        );
    }

    #[test]
    fn a_dashboard_snapshot_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");

        let mut portfolio = PortfolioVm::default();
        portfolio.balance.total_display = "12 345.0000 0000".to_string();
        portfolio.assets.push(chainvue_protocol::AssetVm {
            name: "mambo".to_string(),
            ..chainvue_protocol::AssetVm::default()
        });

        let history = vec![HistoryRowVm {
            txid: "abc".to_string(),
            height: 42,
            ..HistoryRowVm::default()
        }];

        store(&dir).save_snapshot(&portfolio, &history, 1_700_000_000);

        let restored = store(&dir).snapshot().expect("a snapshot");
        assert_eq!(restored.portfolio.balance.total_display, "12 345.0000 0000");
        assert_eq!(restored.portfolio.assets.len(), 1);
        assert_eq!(restored.history.len(), 1);
        assert_eq!(restored.history[0].height, 42);
        assert_eq!(restored.saved_at, 1_700_000_000);
    }

    #[test]
    fn there_is_no_snapshot_before_there_is_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(store(&dir).snapshot().is_none());
        assert!(store(&dir).native_currency().is_none());
        assert!(store(&dir).currency_names().is_empty());
    }

    /// The whole reason the cache is a separate file: it can be deleted, and
    /// nothing the user chose goes with it.
    #[test]
    fn deleting_the_cache_keeps_the_settings() {
        let dir = tempfile::tempdir().expect("tempdir");

        {
            let store = store(&dir);
            store.set_setting("theme", "dark");
            store.remember_native_currency("iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq");
        }

        std::fs::remove_file(dir.path().join("cache.sqlite")).expect("delete the cache");

        let reopened = store(&dir);
        assert_eq!(reopened.setting("theme").as_deref(), Some("dark"));
        assert_eq!(reopened.native_currency(), None);
    }

    #[test]
    fn the_files_are_owner_only() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir = tempfile::tempdir().expect("tempdir");
            let _store = store(&dir);

            for name in ["wallet.sqlite", "cache.sqlite"] {
                let mode = std::fs::metadata(dir.path().join(name))
                    .expect("metadata")
                    .permissions()
                    .mode()
                    & 0o777;
                assert_eq!(mode, 0o600, "{name} is {mode:o}");
            }
        }
    }
}
