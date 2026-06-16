DROP TABLE IF EXISTS custom_address_purchases;

DROP INDEX IF EXISTS idx_users_active_ark_address;
DROP INDEX IF EXISTS idx_users_active_name;
DROP INDEX IF EXISTS idx_user_ark_address;

ALTER TABLE users
    DROP COLUMN IF EXISTS activated_at;

CREATE UNIQUE INDEX idx_user_ark_address ON users (ark_address);
