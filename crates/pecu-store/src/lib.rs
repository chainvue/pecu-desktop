//! What the wallet remembers between runs.
//!
//! # Two databases, because "safe to delete" must be unambiguous
//!
//! **`wallet.sqlite`** holds things that cannot be reconstructed: what the user
//! chose. Settings, the address book, the identities somebody looked up. It
//! also still holds a `node` table, which nothing reads any more — see
//! `migrate::wallet` step 2 for why it is left there rather than dropped. It
//! runs `synchronous = FULL`, and a `user_version` this build does not
//! recognise is a **hard error** — guessing at a schema from the future is how
//! a setting quietly turns into something else.
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
//! reason. It is a JSON file in `pecu-core` and its shape is already a row.
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

use pecu_protocol::{HistoryRowVm, PortfolioVm};
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
    #[error("{} was written by a newer version of Pecu (schema {found}, this build reads {expected})", path.display())]
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

/// A VerusID somebody is keeping an eye on but does not control.
///
/// Name and address only. Everything else about an identity is a chain fact
/// that would be a lie by the time it was read back off disk — a row claiming
/// "Active" for something revoked last week is worse than one that says
/// nothing. The rest is re-read on refresh.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WatchedIdentity {
    pub address: String,
    /// As it read when last looked up. Cosmetic: the address is the identity.
    pub name: String,
}

/// An address this wallet has paid, or been told the name of.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KnownAddress {
    pub address: String,
    /// What the user called it. Empty until they say.
    pub label: String,
    /// What the **chain** calls it: `dude.VRSCTEST@` for an i-address that is a
    /// VerusID. Empty for a plain address, and for one nothing has looked up
    /// yet.
    ///
    /// Distinct from `label`, which is this wallet's private note. A name is a
    /// fact any node will confirm; folding the two together would let a lookup
    /// overwrite something somebody typed.
    pub name: String,
    /// Unix seconds of the last payment, if there has been one.
    pub paid_at: Option<i64>,
    /// How many times this wallet has paid it. Zero for an address that was
    /// named but never used — which is a normal state, not a broken one.
    pub payments: i64,
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

    /// Remove a setting, as distinct from setting it to nothing.
    ///
    /// The difference is load-bearing somewhere already: an empty
    /// `light_server` means "somebody pressed Forget it" and must not be
    /// overridden by the shipped default, while an absent one means nobody has
    /// chosen. Anything that wants to say "no value at all" needs this rather
    /// than an empty string.
    pub fn forget_setting(&self, key: &str) {
        if let Err(error) = self
            .wallet
            .execute("DELETE FROM setting WHERE key = ?1", [key])
        {
            tracing::warn!(%error, key, "a setting could not be removed");
        }
    }

    // There is deliberately no accessor for the `node` table.
    //
    // Endpoints the user configured are not a feature of this build any more,
    // so nothing reads that table and nothing writes it. It is **not** dropped:
    // the migrations here are append-only, and a step that deleted rows
    // somebody chose to keep would be the one kind of migration that cannot be
    // undone by installing an older build. See `migrate::wallet` step 2, which
    // still creates it, and `docs/LATER.md` §14.

    // ── Who this wallet has paid: durable ───────────────────────────────────

    /// Every address this wallet knows about, most recently paid first.
    pub fn known_addresses(&self) -> Vec<KnownAddress> {
        let Ok(mut statement) = self.wallet.prepare(
            "SELECT address, label, paid_at, payments, name FROM address_book
             ORDER BY paid_at DESC NULLS LAST, address",
        ) else {
            return Vec::new();
        };
        let Ok(rows) = statement.query_map([], |row| {
            Ok(KnownAddress {
                address: row.get(0)?,
                label: row.get(1)?,
                paid_at: row.get(2)?,
                payments: row.get(3)?,
                name: row.get(4)?,
            })
        }) else {
            return Vec::new();
        };
        rows.flatten().collect()
    }

    /// Record that a payment to `address` went out.
    ///
    /// Called after a broadcast the node accepted, never before. An address
    /// recorded on an attempt would make the review stop warning about a
    /// recipient the wallet has in fact never successfully paid — which is
    /// precisely the case the warning is for.
    pub fn note_payment(&self, address: &str, at: i64) {
        if let Err(error) = self.wallet.execute(
            "INSERT INTO address_book (address, paid_at, payments) VALUES (?1, ?2, 1)
             ON CONFLICT (address) DO UPDATE SET
                 paid_at  = excluded.paid_at,
                 payments = address_book.payments + 1",
            rusqlite::params![address, at],
        ) {
            tracing::warn!(%error, "a payment could not be recorded against its address");
        }
    }

    /// Record what the chain calls an address.
    ///
    /// Written when a payment resolves a VerusID, and when a lookup fills one
    /// in afterwards. Never touches `label`: that is the owner's note and this
    /// is the chain's answer, and one must not overwrite the other.
    ///
    /// An empty `name` is ignored rather than stored, so a failed lookup cannot
    /// erase a name that was already known.
    pub fn name_address(&self, address: &str, name: &str) {
        if name.is_empty() {
            return;
        }
        if let Err(error) = self.wallet.execute(
            "INSERT INTO address_book (address, name) VALUES (?1, ?2)
             ON CONFLICT (address) DO UPDATE SET name = excluded.name",
            [address, name],
        ) {
            tracing::warn!(%error, "an address could not be given its chain name");
        }
    }

    /// Name an address, or rename it. An empty label clears the name without
    /// forgetting that the address was paid.
    pub fn label_address(&self, address: &str, label: &str) {
        if let Err(error) = self.wallet.execute(
            "INSERT INTO address_book (address, label) VALUES (?1, ?2)
             ON CONFLICT (address) DO UPDATE SET label = excluded.label",
            [address, label],
        ) {
            tracing::warn!(%error, "an address could not be named");
        }
    }

    /// Forget an address entirely.
    ///
    /// Which also forgets that it was ever paid, so the review will warn about
    /// it again. That is the honest consequence of the request rather than a
    /// bug: somebody who removes an address is saying they no longer recognise
    /// it, and being warned next time is what they asked for.
    pub fn forget_address(&self, address: &str) {
        if let Err(error) = self
            .wallet
            .execute("DELETE FROM address_book WHERE address = ?1", [address])
        {
            tracing::warn!(%error, "an address could not be forgotten");
        }
    }

    // ── What a VerusID name meant last time ─────────────────────────────────

    /// The i-address this name resolved to when it was last looked up.
    ///
    /// `None` for a name never seen. That is not the same as "unchanged" and
    /// callers must not treat it as agreement — a first sighting is exactly the
    /// case there is nothing to compare against.
    pub fn identity_address(&self, name: &str) -> Option<String> {
        self.wallet
            .query_row(
                "SELECT address FROM identity_name WHERE name = ?1",
                [name],
                |row| row.get(0),
            )
            .ok()
    }

    /// Record what a name resolved to.
    ///
    /// Overwrites, deliberately. Once the wallet has *shown* somebody that a
    /// name now points somewhere new, holding on to the old value would make
    /// the same warning fire forever — including after they accepted it, which
    /// trains people to click past exactly the alert that matters.
    pub fn remember_identity(&self, name: &str, address: &str, at: i64) {
        if let Err(error) = self.wallet.execute(
            "INSERT INTO identity_name (name, address, seen_at) VALUES (?1, ?2, ?3)
             ON CONFLICT (name) DO UPDATE SET
                 address = excluded.address,
                 seen_at = excluded.seen_at",
            rusqlite::params![name, address, at],
        ) {
            tracing::warn!(%error, "a VerusID's address could not be recorded");
        }
    }

    // ── VerusIDs somebody is keeping an eye on ──────────────────────────────

    /// The identities being watched, newest first.
    ///
    /// Name and address only. Everything else about an identity is a chain fact
    /// that would be a lie by the time it was read back.
    pub fn watched_identities(&self) -> Vec<WatchedIdentity> {
        let Ok(mut statement) = self
            .wallet
            .prepare("SELECT address, name FROM watched_identity ORDER BY seen_at DESC, address")
        else {
            return Vec::new();
        };
        let Ok(rows) = statement.query_map([], |row| {
            Ok(WatchedIdentity {
                address: row.get(0)?,
                name: row.get(1)?,
            })
        }) else {
            return Vec::new();
        };
        rows.filter_map(Result::ok).collect()
    }

    /// Start watching one, or bump it to the top.
    pub fn watch_identity(&self, address: &str, name: &str, at: i64) {
        if let Err(error) = self.wallet.execute(
            "INSERT INTO watched_identity (address, name, seen_at) VALUES (?1, ?2, ?3)
             ON CONFLICT (address) DO UPDATE SET
                 name    = excluded.name,
                 seen_at = excluded.seen_at",
            rusqlite::params![address, name, at],
        ) {
            tracing::warn!(%error, "a VerusID could not be added to the watch list");
        }
    }

    /// Stop watching one.
    pub fn unwatch_identity(&self, address: &str) {
        if let Err(error) = self
            .wallet
            .execute("DELETE FROM watched_identity WHERE address = ?1", [address])
        {
            tracing::warn!(%error, "a VerusID could not be removed from the watch list");
        }
    }

    /// Stop watching all of them.
    pub fn unwatch_all_identities(&self) {
        if let Err(error) = self.wallet.execute("DELETE FROM watched_identity", []) {
            tracing::warn!(%error, "the VerusID watch list could not be cleared");
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

    // ── What the last shielded scan found ───────────────────────────────────

    /// The sealed shielded scan, if one was kept.
    ///
    /// Opaque here, and deliberately so: this layer holds no key and can say
    /// nothing about what is inside. It hands the string back and the caller —
    /// which has the vault — decides whether it opens and whether it belongs to
    /// the account now loaded.
    pub fn shielded_scan(&self) -> Option<String> {
        self.cache
            .query_row("SELECT sealed FROM shielded_scan WHERE id = 1", [], |row| {
                row.get(0)
            })
            .ok()
    }

    /// Keep a sealed shielded scan for the next launch.
    ///
    /// `sealed` must already be ciphertext. Nothing checks that here, because
    /// nothing here could: a string is a string. The guarantee lives at the
    /// call site, which is the only place that has the data key — and
    /// `no_plaintext_shielded_data_reaches_the_cache` asserts it end to end.
    pub fn save_shielded_scan(&self, sealed: &str, now: i64) {
        if let Err(error) = self.cache.execute(
            "INSERT INTO shielded_scan (id, sealed, saved_at) VALUES (1, ?1, ?2)
             ON CONFLICT (id) DO UPDATE SET
                 sealed   = excluded.sealed,
                 saved_at = excluded.saved_at",
            rusqlite::params![sealed, now],
        ) {
            tracing::warn!(%error, "the shielded scan could not be cached");
        }
    }

    /// Throw the kept scan away.
    ///
    /// Called when the thing it describes is no longer trusted — the light
    /// server it came from was removed, or it turned out to belong to a
    /// different account. Not an error path: the next scan rebuilds it.
    pub fn forget_shielded_scan(&self) {
        if let Err(error) = self.cache.execute("DELETE FROM shielded_scan", []) {
            tracing::warn!(%error, "the shielded scan could not be forgotten");
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
        portfolio.assets.push(pecu_protocol::AssetVm {
            name: "mambo".to_string(),
            ..pecu_protocol::AssetVm::default()
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

    #[test]
    fn a_paid_address_is_remembered_and_counted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let address = "RQr2cUkF46n7y8WRzDkd1iV9gHusSSQuzX";

        assert!(store(&dir).known_addresses().is_empty());

        store(&dir).note_payment(address, 1_700_000_000);
        store(&dir).note_payment(address, 1_700_000_100);

        let known = store(&dir).known_addresses();
        assert_eq!(known.len(), 1, "a second payment created a second row");
        assert_eq!(known[0].address, address);
        assert_eq!(known[0].payments, 2);
        assert_eq!(known[0].paid_at, Some(1_700_000_100));
        assert_eq!(known[0].label, "", "nobody has named it yet");
    }

    /// Naming an address must not disturb what the wallet knows about paying
    /// it, and paying it must not wipe the name.
    #[test]
    fn a_name_and_a_payment_history_do_not_overwrite_each_other() {
        let dir = tempfile::tempdir().expect("tempdir");
        let address = "RQr2cUkF46n7y8WRzDkd1iV9gHusSSQuzX";

        store(&dir).label_address(address, "the exchange");
        let known = store(&dir).known_addresses();
        assert_eq!(known[0].label, "the exchange");
        // Named but never paid is a normal state, not a broken one.
        assert_eq!(known[0].payments, 0);
        assert_eq!(known[0].paid_at, None);

        store(&dir).note_payment(address, 1_700_000_000);
        let known = store(&dir).known_addresses();
        assert_eq!(known[0].label, "the exchange", "paying it wiped its name");
        assert_eq!(known[0].payments, 1);

        store(&dir).label_address(address, "somewhere else");
        let known = store(&dir).known_addresses();
        assert_eq!(known[0].label, "somewhere else");
        assert_eq!(known[0].payments, 1, "renaming it reset its history");
    }

    /// The chain's name and the owner's note are two different things, and
    /// neither may overwrite the other.
    ///
    /// A payment to a VerusID records the i-address, so the name is looked up
    /// and written afterwards — while the label, if there is one, was typed by
    /// somebody. Folding them into one column would have let a lookup silently
    /// replace what they wrote.
    #[test]
    fn a_looked_up_name_and_a_typed_label_do_not_overwrite_each_other() {
        let dir = tempfile::tempdir().expect("tempdir");
        let address = "i4YzoP8ZHnh1gNywV9PAT6Yz3AkfXxJmtP";

        store(&dir).label_address(address, "my mate");
        store(&dir).name_address(address, "dude.VRSCTEST@");

        let known = store(&dir).known_addresses();
        assert_eq!(known[0].label, "my mate");
        assert_eq!(known[0].name, "dude.VRSCTEST@");

        // And in the other order, on a fresh row.
        let other = "iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq";
        store(&dir).name_address(other, "someone.VRSCTEST@");
        store(&dir).label_address(other, "the shop");
        let known = store(&dir).known_addresses();
        let row = known
            .iter()
            .find(|row| row.address == other)
            .expect("the row just written");
        assert_eq!(row.label, "the shop");
        assert_eq!(row.name, "someone.VRSCTEST@");
    }

    /// A lookup that came back with nothing must not erase a name that is
    /// already there.
    ///
    /// The failure it guards against is a node that answers for an identity
    /// once and not the next time: the second answer is an absence, not a
    /// correction.
    #[test]
    fn an_empty_name_is_ignored_rather_than_stored() {
        let dir = tempfile::tempdir().expect("tempdir");
        let address = "i4YzoP8ZHnh1gNywV9PAT6Yz3AkfXxJmtP";

        store(&dir).name_address(address, "dude.VRSCTEST@");
        store(&dir).name_address(address, "");

        let known = store(&dir).known_addresses();
        assert_eq!(known.len(), 1);
        assert_eq!(known[0].name, "dude.VRSCTEST@");
    }

    /// Naming an address is not the same as having paid it.
    #[test]
    fn a_name_alone_does_not_claim_a_payment() {
        let dir = tempfile::tempdir().expect("tempdir");
        store(&dir).name_address("i4YzoP8ZHnh1gNywV9PAT6Yz3AkfXxJmtP", "dude.VRSCTEST@");

        let known = store(&dir).known_addresses();
        assert_eq!(known[0].payments, 0);
        assert_eq!(known[0].paid_at, None);
    }

    /// Most recently paid first, with the never-paid ones after — which is the
    /// order somebody scanning the list is looking for.
    #[test]
    fn addresses_are_listed_by_when_they_were_last_paid() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = store(&dir);

        store.note_payment("RVGTY4w2GrdBFrzGaAASBvT6prBr4MxDfJ", 1_700_000_000);
        store.note_payment("RGZbQcWU9LNSa9rat45UMKaeP1q32NBduM", 1_800_000_000);
        store.label_address("RQr2cUkF46n7y8WRzDkd1iV9gHusSSQuzX", "never paid");

        let order: Vec<String> = store
            .known_addresses()
            .into_iter()
            .map(|known| known.address)
            .collect();
        assert_eq!(
            order,
            vec![
                "RGZbQcWU9LNSa9rat45UMKaeP1q32NBduM",
                "RVGTY4w2GrdBFrzGaAASBvT6prBr4MxDfJ",
                "RQr2cUkF46n7y8WRzDkd1iV9gHusSSQuzX",
            ],
        );
    }

    /// Forgetting an address forgets that it was paid, so the review warns
    /// about it again. That is the honest consequence of the request.
    #[test]
    fn forgetting_an_address_forgets_that_it_was_paid() {
        let dir = tempfile::tempdir().expect("tempdir");
        let address = "RQr2cUkF46n7y8WRzDkd1iV9gHusSSQuzX";

        store(&dir).note_payment(address, 1_700_000_000);
        store(&dir).forget_address(address);
        assert!(store(&dir).known_addresses().is_empty());
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

    /// What a VerusID name meant survives a restart, and a change is visible.
    ///
    /// This is the whole value of the table. Nobody can check an i-address by
    /// eye, so the only thing that makes paying `someone@` safer than trusting
    /// one reply from one node is noticing that the reply is not the one from
    /// last time — and noticing needs the last time to still be on disk.
    #[test]
    fn a_verusid_that_moves_is_visible_across_a_restart() {
        let dir = tempfile::tempdir().expect("tempdir");

        {
            let store = store(&dir);
            assert_eq!(
                store.identity_address("someone@"),
                None,
                "a name never seen must not read as agreement",
            );
            store.remember_identity("someone@", "iOLD", 1_700_000_000);
        }

        let reopened = store(&dir);
        assert_eq!(
            reopened.identity_address("someone@").as_deref(),
            Some("iOLD"),
        );

        // It moved. Recording the new value must replace the old one — keeping
        // both would fire the same warning forever, including after somebody
        // accepted it, which is how people learn to click past the one alert
        // that matters.
        reopened.remember_identity("someone@", "iNEW", 1_700_000_100);
        assert_eq!(
            reopened.identity_address("someone@").as_deref(),
            Some("iNEW"),
        );
    }

    /// A watched identity survives a restart, and can be dropped one at a time.
    ///
    /// It used to live in memory only, so somebody who had just looked up the
    /// identity they were about to pay had to look it up again after a restart.
    #[test]
    fn a_watched_identity_outlives_the_session_that_found_it() {
        let dir = tempfile::tempdir().expect("tempdir");

        {
            let store = store(&dir);
            store.watch_identity("iONE", "one.VRSCTEST@", 1_700_000_000);
            store.watch_identity("iTWO", "two.VRSCTEST@", 1_700_000_100);
        }

        let reopened = store(&dir);
        let watched = reopened.watched_identities();
        assert_eq!(watched.len(), 2);
        // Newest first, so the one just looked at is the one at hand.
        assert_eq!(watched[0].address, "iTWO");
        assert_eq!(watched[0].name, "two.VRSCTEST@");

        // One at a time, because a list that can only be emptied wholesale
        // makes keeping one entry cost keeping every entry.
        reopened.unwatch_identity("iTWO");
        let watched = reopened.watched_identities();
        assert_eq!(watched.len(), 1);
        assert_eq!(watched[0].address, "iONE");

        reopened.unwatch_all_identities();
        assert!(reopened.watched_identities().is_empty());
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
