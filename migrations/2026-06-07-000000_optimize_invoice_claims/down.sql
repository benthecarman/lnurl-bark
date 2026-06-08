DROP INDEX IF EXISTS idx_invoice_payment_hash;
DROP INDEX IF EXISTS idx_invoice_pending_expires_at;

ALTER TABLE invoice
    DROP COLUMN IF EXISTS settled_at,
    DROP COLUMN IF EXISTS expires_at,
    DROP COLUMN IF EXISTS created_at,
    DROP COLUMN IF EXISTS payment_hash;
