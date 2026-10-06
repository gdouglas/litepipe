-- meeting_transcript_segments holds the meeting transcript that notes and the
-- MCP transcript tool read, and the text-PII worker never rewrote it, so a
-- secret said aloud in a call was kept in plain text. It is a worker target
-- now. Segments from before this migration are left to an explicit purge, so
-- the worker takes ids above this floor; the partial index keeps each poll to
-- the segments that still need work.
ALTER TABLE meeting_transcript_segments ADD COLUMN redacted_at INTEGER;

INSERT OR IGNORE INTO redaction_floor (table_name, min_id)
    SELECT 'meeting_transcript_segments', COALESCE(MAX(id), 0) FROM meeting_transcript_segments;

CREATE INDEX IF NOT EXISTS idx_meeting_transcript_segments_unredacted
    ON meeting_transcript_segments(id) WHERE redacted_at IS NULL;
