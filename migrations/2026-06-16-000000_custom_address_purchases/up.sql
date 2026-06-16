DROP INDEX IF EXISTS idx_user_ark_address;

ALTER TABLE users
    ADD COLUMN activated_at TIMESTAMP;

UPDATE users
SET activated_at = NOW()
WHERE activated_at IS NULL;

CREATE INDEX idx_user_ark_address ON users (ark_address);
CREATE INDEX idx_users_active_name ON users (name)
    WHERE activated_at IS NOT NULL;
CREATE INDEX idx_users_active_ark_address ON users (ark_address)
    WHERE activated_at IS NOT NULL;

CREATE TABLE custom_address_purchases
(
    invoice_id   INTEGER      NOT NULL PRIMARY KEY references invoice (id) ON DELETE CASCADE,
    user_id      INTEGER      NOT NULL UNIQUE references users (id) ON DELETE CASCADE,
    name         VARCHAR(255) NOT NULL,
    ark_address  TEXT         NOT NULL,
    auth_message TEXT         NOT NULL,
    signature    VARCHAR(128) NOT NULL,
    fee_msats    BIGINT       NOT NULL,
    created_at   TIMESTAMP    NOT NULL DEFAULT NOW(),
    activated_at TIMESTAMP
);

CREATE INDEX idx_custom_address_purchases_user_id ON custom_address_purchases (user_id);
CREATE INDEX idx_custom_address_purchases_ark_address ON custom_address_purchases (ark_address);
