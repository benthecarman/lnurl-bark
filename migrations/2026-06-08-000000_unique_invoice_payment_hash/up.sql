DROP INDEX IF EXISTS idx_invoice_payment_hash;

CREATE UNIQUE INDEX idx_invoice_payment_hash ON invoice (payment_hash)
    WHERE payment_hash IS NOT NULL;
