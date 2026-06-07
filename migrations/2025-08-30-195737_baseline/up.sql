CREATE TABLE users
(
    id            SERIAL PRIMARY KEY,
    pubkey        VARCHAR(66)  NOT NULL,
    name          VARCHAR(255) NOT NULL UNIQUE,
    disabled_zaps BOOLEAN      NOT NULL DEFAULT FALSE
);

CREATE UNIQUE INDEX idx_user_pk ON users (pubkey);
CREATE UNIQUE INDEX idx_user_name ON users (name);

CREATE TABLE invoice
(
    id                 SERIAL PRIMARY KEY,
    user_id            INTEGER       NOT NULL references users (id),
    bolt11             VARCHAR(2048) NOT NULL,
    amount_msats       BIGINT        NOT NULL,
    preimage           VARCHAR(64)   NOT NULL,
    lnurlp_comment     VARCHAR(100),
    state              INTEGER       NOT NULL DEFAULT 0
);

CREATE INDEX idx_invoice_state ON invoice (state);

CREATE TABLE zaps
(
    id       INTEGER NOT NULL PRIMARY KEY references invoice (id),
    request  TEXT    NOT NULL,
    event_id VARCHAR(64)
);

CREATE INDEX idx_zaps_event_id ON zaps (event_id);
