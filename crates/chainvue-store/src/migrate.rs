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
const WALLET_VERSION: i64 = 3;

/// Bumped when a cached table changes shape. Cheap to raise: an unrecognised
/// cache is deleted, not migrated.
const CACHE_VERSION: i64 = 1;

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
            // Endpoints the user added. Durable, because nothing can work out
            // again which node someone chose to trust.
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
             DROP TABLE IF EXISTS snapshot;",
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

        connection
            .execute(
                "INSERT INTO node (label, url) VALUES ('n', 'https://a')",
                [],
            )
            .expect("the node table now exists");
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
