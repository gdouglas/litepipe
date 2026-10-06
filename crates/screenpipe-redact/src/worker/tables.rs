//! Per-table queries used by the reconciliation worker.
//!
//! Destructive-only model: the worker overwrites the source column
//! (`text` / `transcription` / `accessibility_text` / `text_content`)
//! with the redacted text and stamps a `redacted_at` timestamp. There
//! is no separate "redacted text" column; the source IS the redacted
//! text after the UPDATE returns.
//!
//! ## What we redact
//!
//! Four logical surfaces, five [`TargetTable`] variants (UI events
//! split into keyboard vs clipboard):
//!
//! 1. **`ocr_text`** — OCR'd screen text. Source column `text`.
//! 2. **`audio_transcriptions`** — speech-to-text output. Source column
//!    `transcription`.
//! 3. **`frames.accessibility_text`** — accessibility-tree text. The
//!    standalone `accessibility` table was consolidated into `frames`
//!    by `20260312000001_drop_dead_fts_tables.sql`; we redact on the
//!    new home. The "is processed" timestamp is prefixed
//!    (`accessibility_redacted_at`) so the same `frames` row can carry
//!    independent state for accessibility text vs. image redaction.
//! 4. **`ui_events`** — user input events. The same table holds both
//!    typed/keystroke text (`event_type IN ('text', 'key')`) and
//!    clipboard contents (`event_type = 'clipboard'`). Source column
//!    `text_content`. Split into two variants so the row-fetch SQL
//!    can filter by `event_type`.
//!
//! ## "Needs redaction" predicate
//!
//! `<redacted_at_col> IS NULL`. Single bit per row — no version-based
//! re-redaction (the source text is already gone after the first pass,
//! so re-redacting redacted text would be a no-op anyway).

use sqlx::{Row, SqlitePool};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetTable {
    /// OCR'd screen text (`ocr_text.text`).
    Ocr,
    /// Speech-to-text (`audio_transcriptions.transcription`).
    AudioTranscription,
    /// Accessibility-tree text — lives on `frames.accessibility_text`
    /// since the `accessibility` table was consolidated into `frames`
    /// by `20260312000001_drop_dead_fts_tables.sql`. The "is processed"
    /// column is prefixed (`accessibility_redacted_at`) so it doesn't
    /// collide with `frames.image_redacted_at` (image PII worker).
    Accessibility,
    /// Typed text + keystrokes captured via UI events
    /// (`ui_events.text_content` filtered to `event_type IN ('text','key')`).
    UiEventsKeyboard,
    /// Clipboard payloads captured via UI events
    /// (`ui_events.text_content` filtered to `event_type='clipboard'`).
    UiEventsClipboard,
    /// The frame's combined searchable text (`frames.full_text`), which
    /// feeds `frames_fts`. Built from accessibility and OCR text at
    /// insert, so redacting those columns leaves this copy intact.
    FrameFullText,
    /// Per-element screen text (`elements.text`), which feeds
    /// `elements_fts`. Only ids above `redaction_floor` are taken; older
    /// rows are left to an explicit purge (see the 20261005 migration).
    Elements,
}

pub const ALL_TARGET_TABLES: &[TargetTable] = &[
    TargetTable::Ocr,
    TargetTable::AudioTranscription,
    TargetTable::Accessibility,
    TargetTable::UiEventsKeyboard,
    TargetTable::UiEventsClipboard,
    TargetTable::FrameFullText,
    TargetTable::Elements,
];

/// One row to redact.
#[derive(Debug)]
pub struct UnredactedRow {
    pub id: i64,
    pub text: String,
}

impl TargetTable {
    /// Physical SQLite table.
    pub fn table(&self) -> &'static str {
        match self {
            Self::Ocr => "ocr_text",
            Self::AudioTranscription => "audio_transcriptions",
            // accessibility_text lives on frames after the 2026-03-12
            // consolidation; see the variant docs above.
            Self::Accessibility => "frames",
            Self::UiEventsKeyboard | Self::UiEventsClipboard => "ui_events",
            Self::FrameFullText => "frames",
            Self::Elements => "elements",
        }
    }

    /// Source column the redactor reads AND overwrites.
    pub fn source_col(&self) -> &'static str {
        match self {
            Self::Ocr => "text",
            Self::AudioTranscription => "transcription",
            Self::Accessibility => "accessibility_text",
            Self::UiEventsKeyboard | Self::UiEventsClipboard => "text_content",
            Self::FrameFullText => "full_text",
            Self::Elements => "text",
        }
    }

    /// Column holding the unix-seconds timestamp of the last redaction,
    /// used both as the "needs redaction" gate (`IS NULL`) and as
    /// audit metadata. Prefixed for the accessibility variant since
    /// it shares `frames` with the image-redaction worker.
    pub fn redacted_at_col(&self) -> &'static str {
        match self {
            Self::Accessibility => "accessibility_redacted_at",
            Self::FrameFullText => "full_text_redacted_at",
            _ => "redacted_at",
        }
    }

    /// Primary key. `ocr_text` is keyed by `frame_id`; everything
    /// else (including `frames` for the accessibility variant) uses
    /// an autoincrement `id`.
    pub fn pk_col(&self) -> &'static str {
        match self {
            Self::Ocr => "frame_id",
            _ => "id",
        }
    }

    /// Extra `WHERE`-clause filter beyond the redacted-NULL predicate.
    /// Used to slice the `ui_events` table by `event_type`.
    pub fn extra_filter(&self) -> Option<&'static str> {
        match self {
            Self::UiEventsKeyboard => Some("event_type IN ('text','key')"),
            Self::UiEventsClipboard => Some("event_type = 'clipboard'"),
            Self::Elements => Some(
                "id > COALESCE((SELECT min_id FROM redaction_floor WHERE table_name = 'elements'), 0)",
            ),
            _ => None,
        }
    }

    /// Name in `redaction_floor` for tables scanned upward from a moving
    /// floor instead of by an index on the redacted-at column.
    pub fn floor_name(&self) -> Option<&'static str> {
        match self {
            Self::Elements => Some("elements"),
            _ => None,
        }
    }

    /// Stable-ish identifier for logs / status.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Ocr => "ocr_text",
            Self::AudioTranscription => "audio_transcriptions",
            Self::Accessibility => "frames:accessibility_text",
            Self::UiEventsKeyboard => "ui_events:keyboard",
            Self::UiEventsClipboard => "ui_events:clipboard",
            Self::FrameFullText => "frames:full_text",
            Self::Elements => "elements",
        }
    }
}

/// Fetch up to `limit` rows that need redaction. Newest-first — users
/// search recent activity, so the most-likely-to-be-queried rows are
/// reconciled first.
/// Floor-scoped tables (see [`TargetTable::floor_name`]) are oldest-first.
pub async fn fetch_unredacted(
    pool: &SqlitePool,
    table: TargetTable,
    limit: u32,
) -> Result<Vec<UnredactedRow>, sqlx::Error> {
    let extra = table
        .extra_filter()
        .map(|f| format!(" AND {}", f))
        .unwrap_or_default();
    let q = format!(
        "SELECT {pk} AS id, {src} AS text \
         FROM {tbl} \
         WHERE {src} IS NOT NULL AND {src} != '' \
           AND {redacted_at} IS NULL\
           {extra} \
         ORDER BY {pk} {order} \
         LIMIT ?",
        pk = table.pk_col(),
        src = table.source_col(),
        tbl = table.table(),
        redacted_at = table.redacted_at_col(),
        extra = extra,
        // A floor-scoped table is walked upward so the floor can follow it.
        order = if table.floor_name().is_some() {
            "ASC"
        } else {
            "DESC"
        },
    );

    let rows = sqlx::query(&q).bind(limit as i64).fetch_all(pool).await?;
    let out = rows
        .into_iter()
        .map(|r| UnredactedRow {
            id: r.get::<i64, _>("id"),
            text: r.get::<String, _>("text"),
        })
        .collect();
    Ok(out)
}

/// Overwrite the source column with the redacted text and stamp the
/// `redacted_at` timestamp.
///
/// `UPDATE {tbl} SET {src} = ?, {redacted_at} = strftime('%s','now') WHERE {pk} = ?`
///
/// Destructive by design: the raw text is gone after the UPDATE returns.
/// That's the contract of the user-facing "AI PII removal" toggle.
pub async fn write_redacted(
    pool: &SqlitePool,
    table: TargetTable,
    id: i64,
    redacted: &str,
) -> Result<(), sqlx::Error> {
    let q = format!(
        "UPDATE {tbl} SET \
            {src} = ?, \
            {redacted_at_col} = strftime('%s', 'now') \
         WHERE {pk} = ?",
        tbl = table.table(),
        src = table.source_col(),
        redacted_at_col = table.redacted_at_col(),
        pk = table.pk_col(),
    );
    sqlx::query(&q)
        .bind(redacted)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// After a batch is written, move a floor-scoped table's floor up to the
/// highest id in it. Rows at or below that id either were in the batch or
/// had nothing to redact, so the next fetch starts after it. No-op for
/// tables without a floor.
pub async fn advance_floor(
    pool: &SqlitePool,
    table: TargetTable,
    written: &[UnredactedRow],
) -> Result<(), sqlx::Error> {
    let Some(name) = table.floor_name() else {
        return Ok(());
    };
    let Some(top) = written.iter().map(|r| r.id).max() else {
        return Ok(());
    };
    sqlx::query(
        "INSERT INTO redaction_floor (table_name, min_id) VALUES (?1, ?2) \
         ON CONFLICT(table_name) DO UPDATE SET min_id = MAX(min_id, excluded.min_id)",
    )
    .bind(name)
    .bind(top)
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    /// Build an in-memory SQLite DB with just the schema this module needs.
    /// Mirrors the production schema after the 20260507 drop-duplicates
    /// migration: only the `*_redacted_at` "is processed" timestamp
    /// remains; the source column doubles as the redacted output.
    async fn setup() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();

        sqlx::query(
            r#"
            CREATE TABLE ocr_text (
                frame_id INTEGER PRIMARY KEY,
                text TEXT NOT NULL,
                redacted_at INTEGER
            );
            -- Accessibility text now lives on `frames` (the standalone
            -- `accessibility` table was dropped on 2026-03-12). Only the
            -- prefixed timestamp survives the destructive-only refactor.
            CREATE TABLE frames (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                accessibility_text TEXT,
                accessibility_redacted_at INTEGER,
                full_text TEXT,
                full_text_redacted_at INTEGER
            );
            CREATE TABLE elements (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                text TEXT,
                redacted_at INTEGER
            );
            CREATE TABLE redaction_floor (
                table_name TEXT PRIMARY KEY,
                min_id INTEGER NOT NULL
            );
            CREATE TABLE ui_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                event_type TEXT NOT NULL,
                text_content TEXT,
                redacted_at INTEGER
            );
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    #[tokio::test]
    async fn fetch_returns_only_unredacted() {
        let pool = setup().await;
        sqlx::query("INSERT INTO ocr_text (frame_id, text) VALUES (1, 'hi')")
            .execute(&pool)
            .await
            .unwrap();
        // Already-processed row: source overwritten + redacted_at stamped.
        sqlx::query("INSERT INTO ocr_text (frame_id, text, redacted_at) VALUES (2, '[X]', 1)")
            .execute(&pool)
            .await
            .unwrap();

        let rows = fetch_unredacted(&pool, TargetTable::Ocr, 10).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, 1);
    }

    #[tokio::test]
    async fn fetch_skips_empty_text() {
        let pool = setup().await;
        sqlx::query("INSERT INTO ocr_text (frame_id, text) VALUES (1, '')")
            .execute(&pool)
            .await
            .unwrap();
        let rows = fetch_unredacted(&pool, TargetTable::Ocr, 10).await.unwrap();
        assert!(rows.is_empty());
    }

    #[tokio::test]
    async fn write_redacted_overwrites_source_and_stamps_redacted_at() {
        let pool = setup().await;
        sqlx::query("INSERT INTO ocr_text (frame_id, text) VALUES (1, 'alice@example.com')")
            .execute(&pool)
            .await
            .unwrap();

        write_redacted(&pool, TargetTable::Ocr, 1, "[EMAIL]")
            .await
            .unwrap();

        let row = sqlx::query("SELECT text, redacted_at FROM ocr_text WHERE frame_id = 1")
            .fetch_one(&pool)
            .await
            .unwrap();
        let raw: String = row.get(0);
        let when: Option<i64> = row.get(1);
        assert_eq!(raw, "[EMAIL]", "source column must be overwritten");
        assert!(when.is_some(), "redacted_at must be stamped");
    }

    #[tokio::test]
    async fn fetch_orders_newest_first() {
        let pool = setup().await;
        for id in 1..=5 {
            sqlx::query("INSERT INTO ocr_text (frame_id, text) VALUES (?, 'x')")
                .bind(id)
                .execute(&pool)
                .await
                .unwrap();
        }
        let rows = fetch_unredacted(&pool, TargetTable::Ocr, 10).await.unwrap();
        let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
        assert_eq!(ids, vec![5, 4, 3, 2, 1]);
    }

    #[tokio::test]
    async fn ui_events_keyboard_filter_excludes_clipboard() {
        let pool = setup().await;
        sqlx::query("INSERT INTO ui_events (event_type, text_content) VALUES ('text', 'hello')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO ui_events (event_type, text_content) VALUES ('key', 'a')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO ui_events (event_type, text_content) VALUES ('clipboard', 'paste')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let kb = fetch_unredacted(&pool, TargetTable::UiEventsKeyboard, 10)
            .await
            .unwrap();
        assert_eq!(kb.len(), 2);

        let cb = fetch_unredacted(&pool, TargetTable::UiEventsClipboard, 10)
            .await
            .unwrap();
        assert_eq!(cb.len(), 1);
        assert_eq!(cb[0].text, "paste");
    }

    #[tokio::test]
    async fn accessibility_reads_from_frames_accessibility_text() {
        let pool = setup().await;
        sqlx::query(
            "INSERT INTO frames (accessibility_text) VALUES ('AXButton[Send to alice@x.io]')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let rows = fetch_unredacted(&pool, TargetTable::Accessibility, 10)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].text, "AXButton[Send to alice@x.io]");
    }

    #[tokio::test]
    async fn accessibility_writes_overwrite_source_and_stamp_prefixed_timestamp() {
        let pool = setup().await;
        sqlx::query("INSERT INTO frames (accessibility_text) VALUES ('Marcus Chen')")
            .execute(&pool)
            .await
            .unwrap();
        write_redacted(&pool, TargetTable::Accessibility, 1, "[PERSON]")
            .await
            .unwrap();
        let row = sqlx::query(
            "SELECT accessibility_text, accessibility_redacted_at FROM frames WHERE id = 1",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let raw: String = row.get(0);
        let when: Option<i64> = row.get(1);
        assert_eq!(raw, "[PERSON]", "source must be overwritten");
        assert!(when.is_some(), "accessibility_redacted_at must be stamped");
    }

    #[test]
    fn every_captured_text_column_is_a_target() {
        // Each column that stores captured screen, audio or input text,
        // and feeds a search index, has to be rewritten by the worker.
        // A column left out keeps the raw value however good the
        // detectors get: card numbers survived in exactly these two.
        let covered: Vec<(&str, &str)> = ALL_TARGET_TABLES
            .iter()
            .map(|t| (t.table(), t.source_col()))
            .collect();
        for want in [
            ("ocr_text", "text"),
            ("audio_transcriptions", "transcription"),
            ("frames", "accessibility_text"),
            ("frames", "full_text"),
            ("elements", "text"),
            ("ui_events", "text_content"),
        ] {
            assert!(covered.contains(&want), "not redacted: {want:?}");
        }
    }

    #[tokio::test]
    async fn full_text_is_read_and_overwritten_with_its_own_marker() {
        let pool = setup().await;
        sqlx::query(
            "INSERT INTO frames (accessibility_text, accessibility_redacted_at, full_text) \
             VALUES ('[ID]', 1, 'Card 4242424242424242')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let rows = fetch_unredacted(&pool, TargetTable::FrameFullText, 10)
            .await
            .unwrap();
        assert_eq!(
            rows.len(),
            1,
            "accessibility marker must not hide full_text"
        );
        write_redacted(&pool, TargetTable::FrameFullText, rows[0].id, "Card [ID]")
            .await
            .unwrap();
        let (text, when): (String, Option<i64>) =
            sqlx::query_as("SELECT full_text, full_text_redacted_at FROM frames WHERE id = 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(text, "Card [ID]");
        assert!(when.is_some());
    }

    #[tokio::test]
    async fn elements_below_the_floor_are_left_for_the_purge() {
        let pool = setup().await;
        for t in ["old one", "old two", "new one"] {
            sqlx::query("INSERT INTO elements (text) VALUES (?)")
                .bind(t)
                .execute(&pool)
                .await
                .unwrap();
        }
        sqlx::query("INSERT INTO redaction_floor VALUES ('elements', 2)")
            .execute(&pool)
            .await
            .unwrap();
        let rows = fetch_unredacted(&pool, TargetTable::Elements, 10)
            .await
            .unwrap();
        let texts: Vec<&str> = rows.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(texts, ["new one"]);
    }

    #[tokio::test]
    async fn elements_without_a_floor_are_all_covered() {
        let pool = setup().await;
        sqlx::query("INSERT INTO elements (text) VALUES ('a'), ('b')")
            .execute(&pool)
            .await
            .unwrap();
        let rows = fetch_unredacted(&pool, TargetTable::Elements, 10)
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
    }

    #[tokio::test]
    async fn the_elements_floor_moves_past_each_written_batch() {
        // Once the worker has caught up, a fetch that only filters on
        // redacted_at walks every row above a fixed floor on every poll,
        // and elements grows by hundreds of thousands of rows a day. The
        // floor moves past each batch instead, oldest first, so a poll
        // only touches rows it has not seen.
        let pool = setup().await;
        for t in ["a", "b", "c"] {
            sqlx::query("INSERT INTO elements (text) VALUES (?)")
                .bind(t)
                .execute(&pool)
                .await
                .unwrap();
        }
        sqlx::query("INSERT INTO redaction_floor VALUES ('elements', 0)")
            .execute(&pool)
            .await
            .unwrap();

        let first = fetch_unredacted(&pool, TargetTable::Elements, 2)
            .await
            .unwrap();
        let ids: Vec<i64> = first.iter().map(|r| r.id).collect();
        assert_eq!(ids, [1, 2], "oldest first above the floor");
        for r in &first {
            write_redacted(&pool, TargetTable::Elements, r.id, "x")
                .await
                .unwrap();
        }
        advance_floor(&pool, TargetTable::Elements, &first)
            .await
            .unwrap();

        let (floor,): (i64,) =
            sqlx::query_as("SELECT min_id FROM redaction_floor WHERE table_name = 'elements'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(floor, 2);
        let next = fetch_unredacted(&pool, TargetTable::Elements, 2)
            .await
            .unwrap();
        assert_eq!(next.iter().map(|r| r.id).collect::<Vec<_>>(), [3]);
    }
}
