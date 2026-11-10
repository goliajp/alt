-- Permanent holders are selected by repository and digest, independently of
-- their pin identity. Multiple permanent holders remain valid.
CREATE INDEX alt_pins_permanent_content ON alt_pins(repository,blake3)
    WHERE expires_at IS NULL;
