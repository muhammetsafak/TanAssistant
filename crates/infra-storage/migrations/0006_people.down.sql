-- Reverts 0006_people.sql. The two new action_items columns cannot be dropped in place (SQLite
-- refuses to drop a column that carries a foreign key), so the table is rebuilt as it was in
-- 0001. The owner of each item is written back into the old text column from the person's
-- name ("Me" for the "me" person), so the owners survive a downgrade; then the participants
-- and the people are dropped (who took part in which session is lost).
CREATE TABLE action_items_old (
    id       TEXT    NOT NULL PRIMARY KEY,
    note_id  TEXT    NOT NULL REFERENCES notes (id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    task     TEXT    NOT NULL,
    owner    TEXT,
    due      INTEGER,
    done     INTEGER NOT NULL DEFAULT 0 CHECK (done IN (0, 1))
) STRICT;

INSERT INTO action_items_old (id, note_id, position, task, owner, due, done)
SELECT a.id, a.note_id, a.position, a.task,
       COALESCE((SELECT p.display_name FROM people p WHERE p.id = a.owner_person_id), a.owner),
       a.due, a.done
FROM action_items a;

DROP TABLE action_items;
ALTER TABLE action_items_old RENAME TO action_items;
CREATE INDEX idx_action_items_note ON action_items (note_id, position);

DROP INDEX idx_session_participants_person;
DROP TABLE session_participants;
DROP INDEX idx_people_single_self;
DROP TABLE people;
