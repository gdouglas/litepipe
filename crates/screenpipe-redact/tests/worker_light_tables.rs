//! Which redactor the worker uses for which table.
//!
//! The model costs too much per row for `elements`, which gets dozens of rows
//! per frame: run through it, the backlog grew faster than it drained and the
//! engine sat at several cores. `elements` and a frame's `full_text` go
//! through the cheap regex pass, and everything else, meeting transcripts
//! included, through the full one.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use screenpipe_redact::{
    worker::{TargetTable, Worker, WorkerConfig},
    RedactError, RedactionOutput, Redactor,
};
use sqlx::sqlite::SqlitePoolOptions;

/// Replaces every input with a fixed stamp, so the test can see which
/// redactor handled a row.
struct Stamp(&'static str);

#[async_trait]
impl Redactor for Stamp {
    fn name(&self) -> &str {
        self.0
    }
    fn version(&self) -> u32 {
        1
    }
    async fn redact_batch(&self, texts: &[String]) -> Result<Vec<RedactionOutput>, RedactError> {
        Ok(texts
            .iter()
            .map(|t| RedactionOutput {
                input: t.clone(),
                redacted: self.0.to_string(),
                spans: Vec::new(),
            })
            .collect())
    }
}

async fn setup() -> sqlx::SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(2)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query(
        r#"
        CREATE TABLE elements (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            text TEXT,
            redacted_at INTEGER
        );
        CREATE TABLE meeting_transcript_segments (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            transcript TEXT NOT NULL,
            redacted_at INTEGER
        );
        CREATE TABLE redaction_floor (
            table_name TEXT PRIMARY KEY,
            min_id INTEGER NOT NULL
        );
        INSERT INTO redaction_floor VALUES ('meeting_transcript_segments', 1);
        INSERT INTO elements (text) VALUES ('a button label');
        INSERT INTO meeting_transcript_segments (transcript)
            VALUES ('from before the upgrade'), ('said in a call today');
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();
    pool
}

#[tokio::test]
async fn elements_use_the_light_redactor_and_meetings_the_full_one() {
    let pool = setup().await;
    let cfg = WorkerConfig {
        batch_size: 16,
        idle_between_batches: Duration::from_millis(1),
        poll_interval: Duration::from_millis(20),
        tables: vec![TargetTable::Elements, TargetTable::MeetingTranscript],
    };
    let worker = Worker::new(pool.clone(), Arc::new(Stamp("MAIN")), cfg)
        .with_light_redactor(Arc::new(Stamp("LIGHT")));
    let handle = worker.spawn();
    tokio::time::sleep(Duration::from_millis(300)).await;
    handle.abort();

    let (element,): (String,) = sqlx::query_as("SELECT text FROM elements")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(element, "LIGHT");

    let segments: Vec<(String,)> =
        sqlx::query_as("SELECT transcript FROM meeting_transcript_segments ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(segments[0].0, "from before the upgrade", "below the floor");
    assert_eq!(segments[1].0, "MAIN");
}

#[tokio::test]
async fn without_a_light_redactor_every_table_uses_the_full_one() {
    let pool = setup().await;
    let cfg = WorkerConfig {
        batch_size: 16,
        idle_between_batches: Duration::from_millis(1),
        poll_interval: Duration::from_millis(20),
        tables: vec![TargetTable::Elements],
    };
    let handle = Worker::new(pool.clone(), Arc::new(Stamp("MAIN")), cfg).spawn();
    tokio::time::sleep(Duration::from_millis(300)).await;
    handle.abort();

    let (element,): (String,) = sqlx::query_as("SELECT text FROM elements")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(element, "MAIN");
}

/// A frame's `full_text` is its accessibility text and its OCR text joined,
/// and the model already redacts both of those columns. Running it over
/// `full_text` as well doubled the model's work per frame, and it fell
/// behind the frames coming in. `full_text` takes the regex pass instead.
#[tokio::test]
async fn frame_full_text_uses_the_light_redactor_and_accessibility_the_full_one() {
    let pool = SqlitePoolOptions::new()
        .max_connections(2)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query(
        r#"
        CREATE TABLE frames (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            accessibility_text TEXT,
            accessibility_redacted_at INTEGER,
            full_text TEXT,
            full_text_redacted_at INTEGER
        );
        CREATE TABLE redaction_floor (
            table_name TEXT PRIMARY KEY,
            min_id INTEGER NOT NULL
        );
        INSERT INTO redaction_floor VALUES ('frames_full_text', 0);
        INSERT INTO frames (accessibility_text, full_text)
            VALUES ('a window title', 'a window title and its ocr');
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let cfg = WorkerConfig {
        batch_size: 16,
        idle_between_batches: Duration::from_millis(1),
        poll_interval: Duration::from_millis(20),
        tables: vec![TargetTable::Accessibility, TargetTable::FrameFullText],
    };
    let worker = Worker::new(pool.clone(), Arc::new(Stamp("MAIN")), cfg)
        .with_light_redactor(Arc::new(Stamp("LIGHT")));
    let handle = worker.spawn();
    tokio::time::sleep(Duration::from_millis(300)).await;
    handle.abort();

    let (a11y, full): (String, String) =
        sqlx::query_as("SELECT accessibility_text, full_text FROM frames")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(a11y, "MAIN");
    assert_eq!(full, "LIGHT");
}
