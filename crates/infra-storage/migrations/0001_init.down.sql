-- Reverts 0001_init.sql. Destroys all application data; only meant for development and tests.

DROP TRIGGER IF EXISTS search_docs_after_update;
DROP TRIGGER IF EXISTS search_docs_after_delete;
DROP TRIGGER IF EXISTS search_docs_after_insert;
DROP TABLE IF EXISTS search_fts;
DROP TABLE IF EXISTS search_docs;
DROP TABLE IF EXISTS settings;
DROP TABLE IF EXISTS task_assignments;
DROP TABLE IF EXISTS provider_profiles;
DROP TABLE IF EXISTS secrets;
DROP TABLE IF EXISTS variables;
DROP TABLE IF EXISTS action_items;
DROP TABLE IF EXISTS note_frame_refs;
DROP TABLE IF EXISTS note_items;
DROP TABLE IF EXISTS notes;
DROP TABLE IF EXISTS transcript_segments;
DROP TABLE IF EXISTS frames;
DROP TABLE IF EXISTS audio_tracks;
DROP TABLE IF EXISTS sessions;
