//! What the worker writes back when redaction changes nothing.
//!
//! `elements` arrive by the thousand a minute and nearly all of them come
//! back from the regex pass unchanged. Every update to an element fires its
//! full-text index trigger, on any column, so writing each one back, marker
//! and all, would load the database for no change. An unchanged element is
//! not written: the floor moves past it. A table whose floor stays put still
//! needs its marker, or the row would be fetched again, so only the marker is
//! written, which leaves `frames_au` (an update OF full_text) alone.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use screenpipe_redact::{
    worker::{TargetTable, Worker, WorkerConfig, ALL_TARGET_TABLES},
    RedactError, RedactionOutput, Redactor,
};
use sqlx::sqlite::SqlitePoolOptions;

/// Replaces the word SECRET and leaves everything else as it was.
struct Mask;

#[async_trait]
impl Redactor for Mask {
    fn name(&self) -> &str {
        "mask"
    }
    fn version(&self) -> u32 {
        1
    }
    async fn redact_batch(&self, texts: &[String]) -> Result<Vec<RedactionOutput>, RedactError> {
        Ok(texts
            .iter()
            .map(|t| RedactionOutput {
                input: t.clone(),
                redacted: t.replace("SECRET", "[X]"),
                spans: Vec::new(),
            })
            .collect())
    }
}

async fn run(pool: &sqlx::SqlitePool, table: TargetTable) {
    let cfg = WorkerConfig {
        batch_size: 16,
        idle_between_batches: Duration::from_millis(1),
        poll_interval: Duration::from_millis(20),
        tables: vec![table],
    };
    let handle = Worker::new(pool.clone(), Arc::new(Mask), cfg).spawn();
    tokio::time::sleep(Duration::from_millis(300)).await;
    handle.abort();
}

async fn pool() -> sqlx::SqlitePool {
    SqlitePoolOptions::new()
        .max_connections(2)
        .connect("sqlite::memory:")
        .await
        .unwrap()
}

#[tokio::test]
async fn an_unchanged_element_is_not_written_and_the_floor_moves_past_it() {
    let pool = pool().await;
    sqlx::query(
        r#"
        CREATE TABLE elements (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            text TEXT,
            redacted_at INTEGER
        );
        CREATE TABLE redaction_floor (
            table_name TEXT PRIMARY KEY,
            min_id INTEGER NOT NULL
        );
        CREATE TABLE updates (id INTEGER);
        CREATE TRIGGER elements_au AFTER UPDATE ON elements
        BEGIN
            INSERT INTO updates VALUES (NEW.id);
        END;
        INSERT INTO redaction_floor VALUES ('elements', 0);
        INSERT INTO elements (text) VALUES ('a button label'), ('card SECRET');
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    run(&pool, TargetTable::Elements).await;

    let rows: Vec<(String, Option<i64>)> =
        sqlx::query_as("SELECT text, redacted_at FROM elements ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(rows[0], ("a button label".to_string(), None), "unchanged, not written");
    assert_eq!(rows[1].0, "card [X]");
    assert!(rows[1].1.is_some());

    let (updates,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM updates")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(updates, 1, "only the changed element fires its trigger");

    let (floor,): (i64,) =
        sqlx::query_as("SELECT min_id FROM redaction_floor WHERE table_name = 'elements'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(floor, 2, "the floor moves past both rows");
}

#[tokio::test]
async fn an_unchanged_full_text_gets_only_its_marker() {
    let pool = pool().await;
    sqlx::query(
        r#"
        CREATE TABLE frames (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            full_text TEXT,
            full_text_redacted_at INTEGER
        );
        CREATE TABLE redaction_floor (
            table_name TEXT PRIMARY KEY,
            min_id INTEGER NOT NULL
        );
        CREATE TABLE updates (id INTEGER);
        CREATE TRIGGER frames_au AFTER UPDATE OF full_text ON frames
        BEGIN
            INSERT INTO updates VALUES (NEW.id);
        END;
        INSERT INTO redaction_floor VALUES ('frames_full_text', 0);
        INSERT INTO frames (full_text) VALUES ('plain screen text'), ('has SECRET');
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    run(&pool, TargetTable::FrameFullText).await;

    let rows: Vec<(String, Option<i64>)> =
        sqlx::query_as("SELECT full_text, full_text_redacted_at FROM frames ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(rows[0].0, "plain screen text");
    assert!(rows[0].1.is_some(), "marked, so it isn't fetched again");
    assert_eq!(rows[1].0, "has [X]");
    assert!(rows[1].1.is_some());

    let (updates,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM updates")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(updates, 1, "only the changed text touches full_text");
}

/// The engine runs the regex tables in their own worker, so the model's
/// slow batches don't set their pace. Every table belongs to exactly one.
#[test]
fn every_table_goes_to_exactly_one_worker() {
    let (full, light) = TargetTable::partition_by_redactor(ALL_TARGET_TABLES);
    assert_eq!(light, vec![TargetTable::FrameFullText, TargetTable::Elements]);
    assert_eq!(full.len() + light.len(), ALL_TARGET_TABLES.len());
    for t in ALL_TARGET_TABLES {
        assert!(full.contains(t) != light.contains(t), "{t:?} in both or neither");
    }
}
