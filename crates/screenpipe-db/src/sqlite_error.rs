pub(crate) fn is_fatal_sqlite_message(msg_lower: &str) -> bool {
    msg_lower.contains("disk i/o error")
        || msg_lower.contains("malformed")
        // SQLITE_NOTADB (code 26): the file header is unreadable/garbage, so
        // the open handle is unusable. Like "malformed", it never clears on
        // the same connection — treat it as fatal so the batch loop drops the
        // handle instead of cascading "file is not a database" across writes.
        || msg_lower.contains("not a database")
}

pub(crate) fn is_sqlite_connection_error(e: &sqlx::Error) -> bool {
    if matches!(
        e,
        sqlx::Error::Io(_) | sqlx::Error::PoolClosed | sqlx::Error::PoolTimedOut
    ) {
        return true;
    }
    if let sqlx::Error::Database(db) = e {
        return is_fatal_sqlite_message(&db.message().to_lowercase());
    }
    if let sqlx::Error::Protocol(msg) = e {
        return is_fatal_sqlite_message(&msg.to_lowercase());
    }
    false
}

pub(crate) fn is_sqlite_cantopen_error(e: &sqlx::Error) -> bool {
    match e {
        sqlx::Error::Database(db_err) => db_err
            .message()
            .to_lowercase()
            .contains("unable to open database file"),
        _ => false,
    }
}

pub(crate) fn should_recycle_sqlite_connection(e: &sqlx::Error) -> bool {
    is_sqlite_connection_error(e) || is_sqlite_cantopen_error(e)
}

pub(crate) fn is_sqlite_busy_error(e: &sqlx::Error) -> bool {
    match e {
        sqlx::Error::Database(db_err) => {
            let msg = db_err.message().to_lowercase();
            msg.contains("database is locked")
                || msg.contains("database table is locked")
                || msg.contains("busy")
        }
        _ => false,
    }
}

/// `BEGIN IMMEDIATE` on a pooled write connection, leaving the connection
/// clean when it is refused as busy.
///
/// With the SQLite that sqlx 0.7 bundles (3.41.2), a `BEGIN IMMEDIATE` refused
/// as busy leaves its connection inside a transaction. It holds no write
/// lock, but the next `BEGIN` on it fails with "cannot start a transaction
/// within a transaction". Back in the pool, the broken connection reaches the
/// next writer, which spends a retry recovering it. Under contention every
/// write connection breaks in turn, batches run out of retries and their
/// writes (audio chunks, UI events, meeting rows) are dropped. A busy refusal
/// is rolled back here, on the same connection, before anyone else gets it.
pub(crate) async fn begin_immediate(conn: &mut sqlx::SqliteConnection) -> Result<(), sqlx::Error> {
    match sqlx::query("BEGIN IMMEDIATE").execute(&mut *conn).await {
        Ok(_) => Ok(()),
        Err(e) => {
            if is_sqlite_busy_error(&e) {
                // With no transaction open, ROLLBACK only reports that, so
                // its result is not needed.
                let _ = sqlx::query("ROLLBACK").execute(&mut *conn).await;
            }
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fatal_message_recognizes_ioerr_and_corruption() {
        assert!(is_fatal_sqlite_message("disk i/o error"));
        assert!(is_fatal_sqlite_message(
            "error returned from database: (code: 522) disk i/o error"
        ));
        assert!(is_fatal_sqlite_message("database disk image is malformed"));
        assert!(is_fatal_sqlite_message(
            "sqlite failure: database disk image is malformed"
        ));
        assert!(is_fatal_sqlite_message("file is not a database"));
        assert!(is_fatal_sqlite_message(
            "error returned from database: (code: 26) file is not a database"
        ));

        assert!(!is_fatal_sqlite_message("database is locked"));
        assert!(!is_fatal_sqlite_message("no such table: foo"));
        assert!(!is_fatal_sqlite_message("unique constraint failed"));
    }

    #[test]
    fn protocol_wrapped_sqlite_ioerr_is_recyclable() {
        assert!(should_recycle_sqlite_connection(&sqlx::Error::Protocol(
            "error returned from database: (code: 522) disk I/O error".into(),
        )));
        assert!(should_recycle_sqlite_connection(&sqlx::Error::Protocol(
            "database disk image is malformed".into(),
        )));
        assert!(!should_recycle_sqlite_connection(&sqlx::Error::Protocol(
            "database is locked".into(),
        )));
    }

    /// Two writers in WAL mode: A holds the write lock, so B's BEGIN is
    /// refused as busy. Once A commits, B must be able to begin again.
    #[tokio::test]
    async fn a_begin_refused_as_busy_leaves_the_connection_ready() {
        use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode};
        use sqlx::ConnectOptions;
        use std::time::Duration;

        let path = std::env::temp_dir().join(format!("busy_begin_{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let opts = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(Duration::from_millis(100));
        let mut a = opts.clone().connect().await.unwrap();
        let mut b = opts.connect().await.unwrap();
        sqlx::query("CREATE TABLE x (v INTEGER)").execute(&mut a).await.unwrap();

        begin_immediate(&mut a).await.unwrap();
        let refused = begin_immediate(&mut b).await.unwrap_err();
        assert!(is_sqlite_busy_error(&refused), "expected busy, got {refused}");
        sqlx::query("COMMIT").execute(&mut a).await.unwrap();

        let again = begin_immediate(&mut b).await;
        assert!(again.is_ok(), "the next BEGIN failed: {:?}", again.err());
        sqlx::query("ROLLBACK").execute(&mut b).await.unwrap();
        let _ = std::fs::remove_file(&path);
    }
}
