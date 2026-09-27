-- Persist the opaque webauthn-rs ceremony state required to verify a challenge
-- after a restart or on another API instance. NestJS ignores this nullable
-- column while Rust writes it for every challenge it creates.

ALTER TABLE admin_webauthn_challenge
  ADD COLUMN ceremony_state bytea;

ALTER TABLE admin_webauthn_challenge
  ADD CONSTRAINT chk_admin_webauthn_challenge_state
  CHECK (ceremony_state IS NULL OR octet_length(ceremony_state) BETWEEN 1 AND 65536);
