-- 0003_processing: the processing pipeline's state (Phase 10).
--
-- Additive and non-destructive: two new tables and one nullable column. Existing sessions,
-- tracks, notes and transcripts are untouched; sessions processed before this migration simply
-- have no job (the retention policy leaves them alone, since their processing date is unknown).

-- One job per session: how far the pipeline got. Replaced when processing starts over.
CREATE TABLE processing_jobs (
    session_id  TEXT    NOT NULL PRIMARY KEY REFERENCES sessions (id) ON DELETE CASCADE,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL,
    finished_at INTEGER -- set when the last step finished; the retention clock starts here
) STRICT;

-- State of each step of a job. `state` is validated by the domain on read (no CHECK, so a new
-- state never needs a table rebuild); `error` is only set for the 'failed' state.
CREATE TABLE processing_steps (
    session_id TEXT NOT NULL REFERENCES processing_jobs (session_id) ON DELETE CASCADE,
    step       TEXT NOT NULL,
    state      TEXT NOT NULL,
    error      TEXT,
    PRIMARY KEY (session_id, step)
) STRICT;

-- Set when the retention policy deleted the track's audio file. The row stays so the UI can
-- say "audio deleted"; NULL means the file is kept.
ALTER TABLE audio_tracks ADD COLUMN deleted_at INTEGER;
