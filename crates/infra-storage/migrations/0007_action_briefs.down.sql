-- Reverts 0007_action_briefs.sql. The briefs are derived data (they can be generated again),
-- so dropping the table loses nothing that is not reproducible.
DROP TABLE action_briefs;
