-- 0007_action_briefs: the brief the AI writes for one action item (ADR-014 follow-up).
--
-- One brief per action item: the primary key is the item itself, so regenerating overwrites.
-- The brief goes with its action item (and so with its note and session). The profile that
-- wrote it is only a reference for display: deleting the profile keeps the brief.
-- `content_json` is owned by the application layer; the table only stores it.
-- Purely additive: nothing existing is touched.
CREATE TABLE action_briefs (
    action_item_id      TEXT    NOT NULL PRIMARY KEY
                                REFERENCES action_items (id) ON DELETE CASCADE,
    language            TEXT    NOT NULL,
    content_json        TEXT    NOT NULL,
    provider_profile_id TEXT    REFERENCES provider_profiles (id) ON DELETE SET NULL,
    created_at          INTEGER NOT NULL
) STRICT;
