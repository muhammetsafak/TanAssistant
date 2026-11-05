-- 0001_init: baseline schema (ADR-010).
--
-- Conventions
--   * ids are opaque TEXT (UUID v7 minted by the application)
--   * all instants/durations are INTEGER milliseconds
--   * enum-like columns are TEXT validated by the domain on read (no CHECK, so adding a
--     variant never needs a table rebuild)
--   * secret VALUES are never stored here (ADR-007): `secrets` holds metadata only
--
-- The matching down script is 0001_init.down.sql.

CREATE TABLE sessions (
    id            TEXT    NOT NULL PRIMARY KEY,
    kind          TEXT    NOT NULL,
    title         TEXT    NOT NULL,
    started_at    INTEGER NOT NULL,
    ended_at      INTEGER,
    language      TEXT    NOT NULL,
    note_language TEXT    NOT NULL,
    status        TEXT    NOT NULL
) STRICT;

CREATE INDEX idx_sessions_started_at ON sessions (started_at DESC);

CREATE TABLE audio_tracks (
    session_id  TEXT    NOT NULL REFERENCES sessions (id) ON DELETE CASCADE,
    kind        TEXT    NOT NULL,
    path        TEXT    NOT NULL,
    sample_rate INTEGER NOT NULL,
    duration_ms INTEGER NOT NULL,
    PRIMARY KEY (session_id, kind)
) STRICT;

CREATE TABLE frames (
    id              TEXT    NOT NULL PRIMARY KEY,
    session_id      TEXT    NOT NULL REFERENCES sessions (id) ON DELETE CASCADE,
    timestamp_ms    INTEGER NOT NULL,
    path            TEXT    NOT NULL,
    perceptual_hash INTEGER NOT NULL, -- u64 stored bit-for-bit as i64
    description     TEXT
) STRICT;

CREATE INDEX idx_frames_session ON frames (session_id, timestamp_ms);

CREATE TABLE transcript_segments (
    id         INTEGER NOT NULL PRIMARY KEY,
    session_id TEXT    NOT NULL REFERENCES sessions (id) ON DELETE CASCADE,
    track      TEXT    NOT NULL,
    start_ms   INTEGER NOT NULL,
    end_ms     INTEGER NOT NULL,
    language   TEXT,
    text       TEXT    NOT NULL,
    speaker    TEXT
) STRICT;

CREATE INDEX idx_transcript_session ON transcript_segments (session_id, start_ms, id);

CREATE TABLE notes (
    id         TEXT    NOT NULL PRIMARY KEY,
    session_id TEXT    NOT NULL REFERENCES sessions (id) ON DELETE CASCADE,
    language   TEXT    NOT NULL,
    summary    TEXT    NOT NULL,
    created_at INTEGER NOT NULL
) STRICT;

CREATE INDEX idx_notes_session ON notes (session_id, created_at DESC);

-- Ordered decisions and open questions of a note.
CREATE TABLE note_items (
    note_id  TEXT    NOT NULL REFERENCES notes (id) ON DELETE CASCADE,
    kind     TEXT    NOT NULL, -- 'decision' | 'open_question'
    position INTEGER NOT NULL,
    text     TEXT    NOT NULL,
    PRIMARY KEY (note_id, kind, position)
) STRICT;

CREATE TABLE note_frame_refs (
    note_id  TEXT    NOT NULL REFERENCES notes (id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    frame_id TEXT    NOT NULL REFERENCES frames (id) ON DELETE CASCADE,
    PRIMARY KEY (note_id, position)
) STRICT;

CREATE TABLE action_items (
    id       TEXT    NOT NULL PRIMARY KEY,
    note_id  TEXT    NOT NULL REFERENCES notes (id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    task     TEXT    NOT NULL,
    owner    TEXT,
    due      INTEGER,
    done     INTEGER NOT NULL DEFAULT 0 CHECK (done IN (0, 1))
) STRICT;

CREATE INDEX idx_action_items_note ON action_items (note_id, position);

CREATE TABLE variables (
    id          TEXT NOT NULL PRIMARY KEY,
    key         TEXT NOT NULL UNIQUE,
    value       TEXT NOT NULL,
    description TEXT NOT NULL
) STRICT;

-- Secret METADATA only. The value lives in the OS vault (ADR-007); there is intentionally no
-- value column and a schema test guards that.
CREATE TABLE secrets (
    id          TEXT    NOT NULL PRIMARY KEY,
    name        TEXT    NOT NULL UNIQUE,
    description TEXT    NOT NULL,
    created_at  INTEGER NOT NULL
) STRICT;

CREATE TABLE provider_profiles (
    id           TEXT NOT NULL PRIMARY KEY,
    kind         TEXT NOT NULL,
    display_name TEXT NOT NULL,
    model        TEXT NOT NULL,
    base_url     TEXT,
    secret_ref   TEXT REFERENCES secrets (id) ON DELETE SET NULL
) STRICT;

CREATE TABLE task_assignments (
    task       TEXT NOT NULL PRIMARY KEY,
    profile_id TEXT NOT NULL REFERENCES provider_profiles (id) ON DELETE CASCADE
) STRICT;

-- Sparse key/value settings. Missing keys mean "domain default", so defaults live in the
-- domain only and partial updates touch only the keys they mention.
CREATE TABLE settings (
    key   TEXT NOT NULL PRIMARY KEY,
    value TEXT NOT NULL
) STRICT;

-- Full-text search -------------------------------------------------------------------------
-- `search_docs` holds one row per searchable text (a transcript segment or a note). `original`
-- is what is shown; `folded` is the Turkish-folded form (see search.rs) that is indexed.
-- Session deletion cascades here, and the triggers below keep the FTS index in step.
CREATE TABLE search_docs (
    id         INTEGER NOT NULL PRIMARY KEY,
    session_id TEXT    NOT NULL REFERENCES sessions (id) ON DELETE CASCADE,
    source     TEXT    NOT NULL, -- 'transcript' | 'note'
    ref_id     TEXT    NOT NULL, -- note id, or transcript segment rowid
    start_ms   INTEGER,
    original   TEXT    NOT NULL,
    folded     TEXT    NOT NULL
) STRICT;

CREATE INDEX idx_search_docs_session ON search_docs (session_id, source);
CREATE INDEX idx_search_docs_ref ON search_docs (source, ref_id);

CREATE VIRTUAL TABLE search_fts USING fts5 (
    folded,
    content = 'search_docs',
    content_rowid = 'id',
    tokenize = 'unicode61 remove_diacritics 0'
);

CREATE TRIGGER search_docs_after_insert AFTER INSERT ON search_docs BEGIN
    INSERT INTO search_fts (rowid, folded) VALUES (new.id, new.folded);
END;

CREATE TRIGGER search_docs_after_delete AFTER DELETE ON search_docs BEGIN
    INSERT INTO search_fts (search_fts, rowid, folded) VALUES ('delete', old.id, old.folded);
END;

CREATE TRIGGER search_docs_after_update AFTER UPDATE ON search_docs BEGIN
    INSERT INTO search_fts (search_fts, rowid, folded) VALUES ('delete', old.id, old.folded);
    INSERT INTO search_fts (rowid, folded) VALUES (new.id, new.folded);
END;
