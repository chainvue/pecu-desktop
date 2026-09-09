//! Schema versions, and the two different things an unknown one means.
//!
//! Both databases carry their version in SQLite's own `user_version` pragma
//! rather than a table of our own: it is one integer in the file header, it
//! cannot be missing, and it cannot be read as anything else.
//!
//! # Migrations are append-only
//!
//! Each step below moves the schema up by exactly one. To change the shape,
//! add a step — never edit one that has shipped, because someone's file is
//! already at that version and editing history would leave them in a state no
//! version describes.

use std::path::Path;

use rusqlite::Connection;

use crate::StoreError;

/// Bumped when a durable table changes shape.
const WALLET_VERSION: i64 = 6;

/// Bumped when a cached table changes shape. Cheap to raise: an unrecognised
/// cache is deleted, not migrated.
const CACHE_VERSION: i64 = 2;

/// Durable schema. A version from the future is refused.
///
/// The alternative — opening it anyway — means writing a setting into a column
/// that may now mean something else. There is no safe guess, and the honest
/// failure is to say which build wrote it.
pub fn wallet(connection: &Connection, path: &Path) -> Result<(), StoreError> {
    let found = version(connection)?;

    if found > WALLET_VERSION {
        return Err(StoreError::FromTheFuture {
            path: path.to_path_buf(),
            found,
            expected: WALLET_VERSION,
        });
    }

    if found < 1 {
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS setting (
                 key   TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );",
        )?;
    }

    if found < 2 {
        connection.execute_batch(
            // Endpoints the user added. **Nothing reads or writes this table
            // any more** — user-added endpoints are not a feature of this build
            // — and the step is still here because these migrations are
            // append-only and this one shipped.
            //
            // Not dropped, and that is a decision rather than an oversight. A
            // step 7 saying `DROP TABLE node` would delete URLs somebody
            // deliberately configured, and it would raise `WALLET_VERSION`,
            // which makes an older build refuse the file outright — so the
            // deletion would also be the thing that stopped them going back to
            // a build that could still see it. Rows already in here are simply
            // never looked at: `Store` has no accessor for them, `Core::restore`
            // no longer puts them in the running node list, and the
            // `active_node_url` setting naming one of them fails to match a
            // node and leaves the shipped endpoint active, which `restore`
            // already handled and logs.
            //
            // The original reasoning for the shape follows, unchanged, because
            // it is what the table on disk actually is:
            //
            // Durable, because nothing can work out again which node someone
            // chose to trust.
            //
            // `AUTOINCREMENT` rather than a bare rowid: SQLite otherwise reuses
            // the highest id after a delete, and an id that comes back meaning
            // a different endpoint is the sort of thing that is fine until the
            // day it is not.
            //
            // The URL is UNIQUE so the same endpoint cannot be configured
            // twice under two names — the id space would then have two entries
            // that probe identically and disagree about nothing.
            "CREATE TABLE IF NOT EXISTS node (
                 id    INTEGER PRIMARY KEY AUTOINCREMENT,
                 label TEXT NOT NULL,
                 url   TEXT NOT NULL UNIQUE
             );",
        )?;
    }

    if found < 3 {
        connection.execute_batch(
            // Who this wallet has paid.
            //
            // Durable rather than cached, for two reasons. The label is a
            // choice nobody can reconstruct. And `payments` is what makes the
            // review able to say "you have not paid this address before" — a
            // warning that would be worse than useless if it came back after
            // every cache wipe, because a warning that cries wolf is one people
            // learn to click through.
            //
            // The address is the key. It is what the chain agrees on; a label
            // is this wallet's private note about it.
            "CREATE TABLE IF NOT EXISTS address_book (
                 address  TEXT PRIMARY KEY,
                 label    TEXT NOT NULL DEFAULT '',
                 paid_at  INTEGER,
                 payments INTEGER NOT NULL DEFAULT 0
             );",
        )?;
    }

    if found < 4 {
        connection.execute_batch(
            // Which i-address a VerusID name resolved to, and when.
            //
            // Durable, and this is the one table here that is a security
            // control rather than a convenience. A name is not an address: it
            // is a question asked of a node, and the answer is whatever that
            // node says. Nobody can check an i-address by eye any better than
            // they can check a name, so the only thing that makes paying
            // `someone@` safer than trusting one reply is **noticing when the
            // reply changes**.
            //
            // Keyed by name, because the question being asked is "did this name
            // mean something else last time".
            "CREATE TABLE IF NOT EXISTS identity_name (
                 name    TEXT PRIMARY KEY,
                 address TEXT NOT NULL,
                 seen_at INTEGER NOT NULL
             );",
        )?;
    }

    if found < 5 {
        connection.execute_batch(
            // VerusIDs somebody looked up that are not their own.
            //
            // Durable, because looking one up is a decision and nothing can
            // reconstruct it — the same reason `node` and `address_book` are
            // durable. An earlier version kept these in memory only, and they
            // vanished on restart; somebody who had just looked up the identity
            // they were about to pay had to look it up again.
            //
            // Only the name and the address, deliberately. Status and timelock
            // are chain facts that go stale on disk, and a row claiming
            // "Active" for something revoked last week would be worse than one
            // that says nothing. They are re-read on refresh.
            "CREATE TABLE IF NOT EXISTS watched_identity (
                 address TEXT PRIMARY KEY,
                 name    TEXT NOT NULL DEFAULT '',
                 seen_at INTEGER NOT NULL
             );",
        )?;
    }

    if found < 6 {
        connection.execute_batch(
            // What the chain calls an address, as distinct from what its owner
            // calls it.
            //
            // A payment to a VerusID records the **i-address** it resolved to,
            // because that is what the transaction pays and what can be checked
            // afterwards. So the address book filled up with `i4YzoP8Z…` rows
            // for people the user knows as `dude.VRSCTEST@`, and the send
            // screen showed them that way.
            //
            // Separate from `label`, and not folded into it, because they are
            // different claims. `label` is a private note this wallet's owner
            // wrote; `name` is a fact about the chain that any node will
            // confirm. Writing a looked-up name into `label` would silently
            // overwrite something somebody typed.
            "ALTER TABLE address_book ADD COLUMN name TEXT NOT NULL DEFAULT '';",
        )?;
    }

    set_version(connection, WALLET_VERSION)
}

/// Cached schema. A version this build does not know is **wiped**, because
/// everything in here can be asked for again and the worst case is one slow
/// refresh.
///
/// The wipe happens by dropping the tables rather than the file, so the caller
/// keeps its open connection. `Store::open` also handles a file too broken to
/// open at all.
pub fn cache(connection: &Connection) -> Result<(), StoreError> {
    let found = version(connection)?;

    if found > CACHE_VERSION {
        tracing::info!(
            found,
            expected = CACHE_VERSION,
            "the cache was written by a newer build; rebuilding it",
        );
        connection.execute_batch(
            "DROP TABLE IF EXISTS kv;
             DROP TABLE IF EXISTS currency_name;
             DROP TABLE IF EXISTS snapshot;
             DROP TABLE IF EXISTS shielded_scan;",
        )?;
        set_version(connection, 0)?;
    }

    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS kv (
             key   TEXT PRIMARY KEY,
             value TEXT NOT NULL
         );

         -- A currency's name is fixed when it is registered and can never
         -- change, so nothing here ever needs invalidating. This is the cache
         -- that matters most: naming currencies costs one request each.
         CREATE TABLE IF NOT EXISTS currency_name (
             currency TEXT PRIMARY KEY,
             name     TEXT NOT NULL
         );

         -- Exactly one row. The dashboard as it looked when the wallet last
         -- ran, so a cold start shows figures rather than nothing.
         CREATE TABLE IF NOT EXISTS snapshot (
             id        INTEGER PRIMARY KEY CHECK (id = 1),
             portfolio TEXT NOT NULL,
             history   TEXT NOT NULL,
             saved_at  INTEGER NOT NULL
         );

         -- Exactly one row: what the last shielded scan found, sealed under the
         -- vault's data key.
         --
         -- Here rather than in the durable database, and the reasoning is worth
         -- writing down because it looks like the wrong choice. A scan costs
         -- minutes, so losing it hurts — but it is *derived*: every byte can be
         -- recomputed by asking a light server again, which is exactly the
         -- contract of this database. The durable one is for things nothing
         -- can reconstruct, and the birthday of a wallet (which cannot) lives
         -- there as a setting.
         --
         -- Only `sealed` carries anything. Which account it belongs to is
         -- inside the ciphertext, not in a column, so a row here says that a
         -- shielded scan exists and nothing else — no address, no balance, no
         -- height. The one leak left is that the wallet has a shielded account
         -- at all, which the vault already shows.
         CREATE TABLE IF NOT EXISTS shielded_scan (
             id       INTEGER PRIMARY KEY CHECK (id = 1),
             sealed   TEXT NOT NULL,
             saved_at INTEGER NOT NULL
         );",
    )?;

    set_version(connection, CACHE_VERSION)
}

fn version(connection: &Connection) -> Result<i64, StoreError> {
    Ok(connection.query_row("PRAGMA user_version", [], |row| row.get(0))?)
}

fn set_version(connection: &Connection, version: i64) -> Result<(), StoreError> {
    // `user_version` does not accept a bound parameter, so this is formatted —
    // safe because `version` is an `i64` from a constant in this file and can
    // never carry anything but digits.
    connection.execute_batch(&format!("PRAGMA user_version = {version};"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_database_lands_on_the_current_version() {
        let connection = Connection::open_in_memory().expect("memory");
        wallet(&connection, Path::new("wallet.sqlite")).expect("migrate");
        assert_eq!(version(&connection).expect("version"), WALLET_VERSION);

        // Running it again is a no-op, which is what makes every start safe.
        wallet(&connection, Path::new("wallet.sqlite")).expect("migrate again");
        assert_eq!(version(&connection).expect("version"), WALLET_VERSION);
    }

    /// The migrations are append-only, so a file written by an older build has
    /// to arrive at the same schema as a fresh one — with what it already held
    /// still in it.
    #[test]
    fn a_database_from_an_older_build_is_migrated_in_place() {
        let connection = Connection::open_in_memory().expect("memory");

        // Version 1, exactly as the first build left it.
        connection
            .execute_batch(
                "CREATE TABLE setting (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 INSERT INTO setting (key, value) VALUES ('auto_lock_minutes', '15');",
            )
            .expect("v1 schema");
        set_version(&connection, 1).expect("set");

        wallet(&connection, Path::new("wallet.sqlite")).expect("migrate");

        assert_eq!(version(&connection).expect("version"), WALLET_VERSION);

        let kept: String = connection
            .query_row(
                "SELECT value FROM setting WHERE key = 'auto_lock_minutes'",
                [],
                |row| row.get(0),
            )
            .expect("the setting survived the migration");
        assert_eq!(kept, "15");

        // Still created, still writable, and deliberately never read by this
        // build — see the comment on step 2. The assertion is kept because the
        // step is kept: an upgrader's rows have to survive arriving at the
        // current version, or "we simply stop looking at it" would not be a
        // true description of what happens to them.
        connection
            .execute(
                "INSERT INTO node (label, url) VALUES ('n', 'https://a')",
                [],
            )
            .expect("the node table is still created, unread");
        connection
            .execute("INSERT INTO address_book (address) VALUES ('R…')", [])
            .expect("the address book now exists");
    }

    /// A settings file from a newer build must not be opened. Guessing at a
    /// schema from the future is how a preference turns into something else.
    #[test]
    fn a_durable_database_from_the_future_is_refused() {
        let connection = Connection::open_in_memory().expect("memory");
        set_version(&connection, WALLET_VERSION + 5).expect("set");

        let error = wallet(&connection, Path::new("wallet.sqlite")).expect_err("refused");
        assert!(
            matches!(error, StoreError::FromTheFuture { .. }),
            "{error:?}"
        );
    }

    /// The cache makes the opposite choice, and can afford to: everything in
    /// it can be asked for again.
    #[test]
    fn a_cache_from_the_future_is_rebuilt_rather_than_refused() {
        let connection = Connection::open_in_memory().expect("memory");
        cache(&connection).expect("migrate");
        connection
            .execute("INSERT INTO kv (key, value) VALUES ('a', 'b')", [])
            .expect("insert");

        set_version(&connection, CACHE_VERSION + 5).expect("set");
        cache(&connection).expect("a future cache is rebuilt, not refused");

        assert_eq!(version(&connection).expect("version"), CACHE_VERSION);
        let rows: i64 = connection
            .query_row("SELECT count(*) FROM kv", [], |row| row.get(0))
            .expect("count");
        assert_eq!(rows, 0, "the stale cache survived the rebuild");
    }
}
