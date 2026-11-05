-- Reverts 0002_provider_vision.sql: drops the column (the vision flag of Ollama profiles is lost).
ALTER TABLE provider_profiles DROP COLUMN supports_vision;
