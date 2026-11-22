-- 0005_track_mutes: the spans of a recording during which a channel was muted (ADR-014).
--
-- Additive: one new table, nothing existing changes. Times are milliseconds on the active
-- time axis of the recording (paused time is not counted). `end_ms` is NULL while the channel
-- is still muted; a crash leaves it NULL and recovery closes it. Rows go with their session.
CREATE TABLE track_mutes (
    id         INTEGER NOT NULL PRIMARY KEY,
    session_id TEXT    NOT NULL REFERENCES sessions (id) ON DELETE CASCADE,
    kind       TEXT    NOT NULL,
    start_ms   INTEGER NOT NULL,
    end_ms     INTEGER,
    CHECK (end_ms IS NULL OR end_ms >= start_ms)
) STRICT;

CREATE INDEX idx_track_mutes_session ON track_mutes (session_id, start_ms, id);
