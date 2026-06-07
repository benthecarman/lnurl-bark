DROP INDEX IF EXISTS idx_user_ark_address;

ALTER TABLE users
    ALTER COLUMN ark_address TYPE VARCHAR(66);

ALTER TABLE users
    RENAME COLUMN ark_address TO pubkey;

CREATE UNIQUE INDEX idx_user_pk ON users (pubkey);
