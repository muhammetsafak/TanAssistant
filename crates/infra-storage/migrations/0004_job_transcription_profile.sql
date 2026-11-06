-- 0004_job_transcription_profile: a transcription profile chosen for one processing job.
--
-- Additive and non-destructive: one nullable column. NULL (every existing job) means the job
-- uses the general transcription assignment, as before. There is deliberately no foreign key to
-- the provider profiles: a deleted profile must not delete or block the job; the pipeline
-- falls back to the general assignment when the stored profile is gone.
ALTER TABLE processing_jobs ADD COLUMN transcription_profile_id TEXT;
