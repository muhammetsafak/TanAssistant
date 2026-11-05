-- Whether a provider profile's model understands images. Only Ollama's answer is taken from
-- this column (it depends on the served model); for the other providers the application's
-- capability matrix decides, and the backfill below just keeps existing rows in line with it.
-- Additive and non-destructive: existing rows keep every other value.
ALTER TABLE provider_profiles
    ADD COLUMN supports_vision INTEGER NOT NULL DEFAULT 0 CHECK (supports_vision IN (0, 1));

UPDATE provider_profiles
SET supports_vision = 1
WHERE kind IN ('openai', 'anthropic', 'gemini');
