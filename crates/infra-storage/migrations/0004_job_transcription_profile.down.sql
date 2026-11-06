-- Reverts 0004_job_transcription_profile.sql: drops the column (the per-job choice is lost;
-- jobs, steps and everything else are untouched).
ALTER TABLE processing_jobs DROP COLUMN transcription_profile_id;
