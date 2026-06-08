# lnurl-bark

`lnurl-bark` is a small LNURL-pay server for Bark. It lets users register a
Lightning address name backed by an Ark address, serves LNURL-pay metadata, and
asks `barkd` to generate BOLT11 invoices for incoming payments.

The service also stores invoices and optional Nostr zap requests in Postgres,
then periodically checks `barkd` for paid invoices and marks them settled.

## Features

- LNURL-pay metadata at `/.well-known/lnurlp/:name`
- Invoice generation at `/get-invoice/:name`
- User registration at `/v1/register`
- Optional Nostr zap request storage
- Postgres persistence through Diesel migrations
- `barkd` REST API integration

## Prerequisites

- Rust toolchain
- Postgres
- Diesel CLI with Postgres support
- A reachable `barkd` REST API
- A Nostr `nsec` key for zap metadata

Install Diesel CLI if needed:

```sh
cargo install diesel_cli --no-default-features --features postgres
```

## Configuration

The binary reads command-line flags and environment variables. It also loads a
local `.env` file on startup.

| Variable | Required | Default | Description |
| --- | --- | --- | --- |
| `LNURL_PG_URL` | yes | | Postgres connection string used by the application |
| `LNURL_NSEC` | yes | | Nostr secret key used for zap support |
| `LNURL_BARKD_URL` | yes | | Base URL for the `barkd` REST API |
| `LNURL_BARKD_TOKEN` | no | | Bearer token for the `barkd` REST API |
| `LNURL_BIND` | no | `0.0.0.0` | HTTP bind address |
| `LNURL_PORT` | no | `3000` | HTTP port |
| `LNURL_NETWORK` | no | `bitcoin` | Bitcoin network |
| `LNURL_MIN_SENDABLE` | no | `1000` | Minimum LNURL amount in millisatoshis |
| `LNURL_MAX_SENDABLE` | no | `11000000000` | Maximum LNURL amount in millisatoshis |
| `LNURL_DOMAIN` | no | `localhost:3000` | Public domain used in LNURL callbacks and Lightning addresses |

Example `.env`:

```env
LNURL_PG_URL=postgres://postgres:postgres@127.0.0.1:5432/lnurl_bark
LNURL_NSEC=nsec...
LNURL_BARKD_URL=http://127.0.0.1:3535
LNURL_BARKD_TOKEN=
LNURL_DOMAIN=example.com
LNURL_PORT=3000
```

For Diesel CLI commands, also set `DATABASE_URL` to the same Postgres URL:

```sh
export DATABASE_URL="$LNURL_PG_URL"
```

## Database Setup

Create the database, then run migrations:

```sh
createdb lnurl_bark
diesel migration run
```

The migrations create `users`, `invoice`, and `zaps` tables.

## Running

```sh
cargo run
```

By default the server listens on `0.0.0.0:3000`.

You can also pass configuration as flags:

```sh
cargo run -- \
  --pg-url postgres://postgres:postgres@127.0.0.1:5432/lnurl_bark \
  --nsec nsec... \
  --barkd-url http://127.0.0.1:3535 \
  --domain example.com
```

## API

### Health Check

```http
GET /health-check
```

Returns a simple health response:

```json
{
  "status": "pass",
  "version": "0"
}
```

### Register User

```http
POST /v1/register
Content-Type: application/json

{
  "name": "alice",
  "ark_address": "ark..."
}
```

The registered user can receive payments at the Lightning address
`alice@example.com` when `LNURL_DOMAIN=example.com`.

### LNURL-Pay Metadata

```http
GET /.well-known/lnurlp/alice
```

Returns the LNURL-pay callback, amount limits, metadata, and Nostr zap support
information for the registered user.

### Generate Invoice

```http
GET /get-invoice/alice?amount=1000
```

`amount` is required and is denominated in millisatoshis. Bark invoices must be
for whole sats, so the amount must be divisible by `1000`.

Optional query parameters:

- `comment`: LNURL-pay comment, up to 100 characters
- `nostr`: serialized Nostr zap request event

Successful responses include a BOLT11 invoice in the `pr` field.

## Development

Run the test suite:

```sh
cargo test
```

Database-backed migration/model tests are enabled when `LNURL_TEST_DATABASE_URL`
points at a Postgres database the test process can create schemas in:

```sh
LNURL_TEST_DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/lnurl_bark_test cargo test
```

These tests create and drop isolated temporary schemas inside that database.

Format the code:

```sh
cargo fmt
```

Check the project:

```sh
cargo check
```
