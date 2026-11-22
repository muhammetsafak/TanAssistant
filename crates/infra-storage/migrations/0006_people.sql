-- 0006_people: the person directory, the participants of sessions, and an action item's owner
-- as a reference to a person instead of free text (ADR-014).
--
-- Schema only. Existing owner texts are moved to people by the application on start-up
-- (SQLite's lower() only folds ASCII, so it would store "Özge" and "ÖZGE" as two people); the
-- old `action_items.owner` column stays, unread, so this migration is purely additive.
--
-- `people.normalized_name` is the application's normalized form of the name and is unique:
-- one person per name. The "me" person (the one who makes the recordings) is seeded here; its
-- normalized name is empty, which no real name can normalize to, so it never collides.
CREATE TABLE people (
    id              TEXT    NOT NULL PRIMARY KEY,
    display_name    TEXT    NOT NULL,
    normalized_name TEXT    NOT NULL UNIQUE,
    is_self         INTEGER NOT NULL DEFAULT 0 CHECK (is_self IN (0, 1)),
    created_at      INTEGER NOT NULL
) STRICT;

-- At most one "me" person.
CREATE UNIQUE INDEX idx_people_single_self ON people (is_self) WHERE is_self = 1;

INSERT INTO people (id, display_name, normalized_name, is_self, created_at)
VALUES ('self', 'Me', '', 1, CAST(strftime('%s', 'now') AS INTEGER) * 1000);

-- Rows go with their session or their person. `source` is who added the participant: the AI
-- while writing the note (replaced when the note is regenerated) or the user (kept).
CREATE TABLE session_participants (
    session_id TEXT NOT NULL REFERENCES sessions (id) ON DELETE CASCADE,
    person_id  TEXT NOT NULL REFERENCES people (id) ON DELETE CASCADE,
    source     TEXT NOT NULL CHECK (source IN ('ai', 'manual')),
    PRIMARY KEY (session_id, person_id)
) STRICT;

CREATE INDEX idx_session_participants_person ON session_participants (person_id);

-- Deleting a person leaves their action items ownerless instead of deleting them.
ALTER TABLE action_items
    ADD COLUMN owner_person_id TEXT REFERENCES people (id) ON DELETE SET NULL;
ALTER TABLE action_items
    ADD COLUMN owner_edited INTEGER NOT NULL DEFAULT 0 CHECK (owner_edited IN (0, 1));

CREATE INDEX idx_action_items_owner ON action_items (owner_person_id);
