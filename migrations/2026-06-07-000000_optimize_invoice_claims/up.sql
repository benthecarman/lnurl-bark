ALTER TABLE invoice
    ADD COLUMN payment_hash VARCHAR(64),
    ADD COLUMN created_at TIMESTAMP NOT NULL DEFAULT NOW(),
    ADD COLUMN expires_at TIMESTAMP,
    ADD COLUMN settled_at TIMESTAMP;

CREATE INDEX idx_invoice_pending_expires_at ON invoice (expires_at)
    WHERE state = 0;

CREATE INDEX idx_invoice_payment_hash ON invoice (payment_hash)
    WHERE payment_hash IS NOT NULL;
