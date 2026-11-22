-- Reverts 0005_track_mutes.sql: drops the table (the recorded mute spans are lost; sessions
-- and their audio files are untouched).
DROP INDEX idx_track_mutes_session;
DROP TABLE track_mutes;
