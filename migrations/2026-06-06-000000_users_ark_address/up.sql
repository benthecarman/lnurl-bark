DROP INDEX IF EXISTS idx_user_pk;

ALTER TABLE users
    RENAME COLUMN pubkey TO ark_address;

ALTER TABLE users
    ALTER COLUMN ark_address TYPE TEXT;

CREATE UNIQUE INDEX idx_user_ark_address ON users (ark_address);
