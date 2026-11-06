-- Reverts 0003_processing.sql: drops the job tables and the deleted-audio marker (the marker is
-- lost; sessions, tracks and notes are untouched).
DROP TABLE processing_steps;
DROP TABLE processing_jobs;
ALTER TABLE audio_tracks DROP COLUMN deleted_at;
