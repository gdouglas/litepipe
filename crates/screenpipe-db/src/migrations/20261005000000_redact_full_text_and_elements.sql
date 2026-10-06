-- The async text-PII worker rewrote frames.accessibility_text, ocr_text,
-- audio_transcriptions and ui_events, but never frames.full_text or
-- elements.text. Both hold the same captured screen text and both feed a
-- search index (frames_fts, elements_fts), so a value the worker removed
-- elsewhere stayed searchable here.

ALTER TABLE frames ADD COLUMN full_text_redacted_at INTEGER;
CREATE INDEX IF NOT EXISTS idx_frames_full_text_redacted_at ON frames(full_text_redacted_at);

ALTER TABLE elements ADD COLUMN redacted_at INTEGER;

-- elements can hold tens of millions of rows. Rows written before this
-- migration are left to an explicit purge rather than a background
-- backfill, so the worker only takes ids above this floor. A fresh
-- database has no elements, so its floor is 0 and everything is covered.
CREATE TABLE IF NOT EXISTS redaction_floor (
    table_name TEXT PRIMARY KEY,
    min_id INTEGER NOT NULL
);
INSERT OR IGNORE INTO redaction_floor (table_name, min_id)
    SELECT 'elements', COALESCE(MAX(id), 0) FROM elements;
