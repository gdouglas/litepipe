-- Redacting frames.full_text for every frame stored before the upgrade ran
-- the text model at about one frame a second, days of background CPU. Like
-- elements, older frames are left to an explicit purge: the worker only takes
-- frames above this floor. The floor does not move, because OCR can fill a
-- frame's full_text after the row exists. The partial index keeps each poll
-- to the frames that still need work.
INSERT OR IGNORE INTO redaction_floor (table_name, min_id)
    SELECT 'frames_full_text', COALESCE(MAX(id), 0) FROM frames;

CREATE INDEX IF NOT EXISTS idx_frames_full_text_unredacted
    ON frames(id) WHERE full_text_redacted_at IS NULL;

-- Superseded by the partial index above.
DROP INDEX IF EXISTS idx_frames_full_text_redacted_at;
