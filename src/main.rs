use anyhow::Context;
use axum::extract::DefaultBodyLimit;
use axum::http::Method;
use axum::middleware;
use axum::routing::{get, post};
use axum::{http, Extension, Router};
use clap::Parser;
use diesel::r2d2::{ConnectionManager, Pool};
use diesel::PgConnection;
use diesel_migrations::{embed_migrations, EmbeddedMigrations, MigrationHarness};
use log::{error, info, warn};
use nostr::Keys;
use std::collections::HashSet;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tower_http::cors::{Any, CorsLayer};
use tower_http::timeout::TimeoutLayer;

use crate::barkd::BarkdClient;
use crate::config::*;
use crate::models::invoice::{Invoice, InvoiceState};
use crate::rate_limit::{rate_limit_middleware, RateLimiter};
use crate::routes::*;

mod barkd;
mod config;
mod models;
mod rate_limit;
mod routes;

const MIGRATIONS: EmbeddedMigrations = embed_migrations!();

#[derive(Clone)]
pub struct State {
    pub db_pool: Pool<ConnectionManager<PgConnection>>,
    pub keys: Keys,
    pub barkd: Arc<BarkdClient>,

    // -- config options --
    pub domain: String,
    pub min_sendable: u64,
    pub max_sendable: u64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenv::dotenv().ok();
    pretty_env_logger::try_init()?;
    let config: Config = Config::parse();

    let keys = Keys::from_str(&config.nsec)?;

    let manager = ConnectionManager::<PgConnection>::new(config.pg_url.clone());
    let db_pool = Pool::builder()
        .max_size(10) // should be a multiple of 100, our database connection limit
        .test_on_check_out(true)
        .build(manager)
        .expect("Unable to build DB connection pool");
    run_migrations(&db_pool)?;

    let barkd = Arc::new(BarkdClient::new(
        config.barkd_url.clone(),
        config.barkd_token.clone(),
    )?);
    let rate_limiter = Arc::new(RateLimiter::new(config.rate_limit_per_minute));
    let request_timeout = Duration::from_secs(config.request_timeout_seconds);
    let max_request_body_bytes = config.max_request_body_bytes;

    let state = State {
        db_pool: db_pool.clone(),
        keys: keys.clone(),
        barkd,
        domain: config.domain,
        min_sendable: config.min_sendable,
        max_sendable: config.max_sendable,
    };

    tokio::spawn(claim_paid_invoices(state.clone()));

    let addr: std::net::SocketAddr = format!("{}:{}", config.bind, config.port)
        .parse()
        .expect("Failed to parse bind/port for webserver");

    println!("Webserver running on http://{addr}");

    let server_router = Router::new()
        .route("/health-check", get(health_check))
        .route("/get-invoice/:hash", get(get_invoice))
        .route("/verify/:desc_hash/:pay_hash", get(verify))
        .route("/.well-known/lnurlp/:name", get(get_lnurl_pay))
        .route("/v1/register", post(register_route))
        .fallback(fallback)
        .layer(Extension(state.clone()))
        .layer(middleware::from_fn_with_state(
            rate_limiter,
            rate_limit_middleware,
        ))
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_headers([http::header::CONTENT_TYPE, http::header::AUTHORIZATION])
                .allow_methods([
                    Method::GET,
                    Method::POST,
                    Method::PUT,
                    Method::DELETE,
                    Method::OPTIONS,
                ]),
        )
        .layer(TimeoutLayer::new(request_timeout))
        .layer(DefaultBodyLimit::max(max_request_body_bytes));

    let server = axum::Server::bind(&addr)
        .serve(server_router.into_make_service_with_connect_info::<SocketAddr>());

    // todo Invoice event stream for zaps

    let graceful = server.with_graceful_shutdown(async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to create Ctrl+C shutdown signal");
    });

    // Await the server to receive the shutdown signal
    if let Err(e) = graceful.await {
        eprintln!("shutdown error: {e}");
    }

    Ok(())
}

fn run_migrations(pool: &Pool<ConnectionManager<PgConnection>>) -> anyhow::Result<()> {
    let mut conn = pool
        .get()
        .context("failed to get DB connection for migrations")?;
    conn.run_pending_migrations(MIGRATIONS)
        .map_err(|e| anyhow::anyhow!("failed to run database migrations: {e}"))?;
    Ok(())
}

async fn claim_paid_invoices(state: State) {
    let mut interval = tokio::time::interval(Duration::from_secs(30));

    loop {
        interval.tick().await;
        if let Err(e) = claim_paid_invoices_once(&state).await {
            error!("Unable to claim paid invoices: {e:#}");
        }
    }
}

async fn claim_paid_invoices_once(state: &State) -> anyhow::Result<()> {
    let invoices = {
        let mut conn = state.db_pool.get()?;
        let cancelled = Invoice::cancel_expired_pending(&mut conn)?;
        if cancelled > 0 {
            info!("Cancelled {cancelled} expired invoices");
        }
        Invoice::get_by_state(&mut conn, InvoiceState::Pending as i32)?
    };

    let bark_pending_hashes = match state.barkd.pending_receives().await {
        Ok(receives) => Some(
            receives
                .into_iter()
                .map(|receive| receive.payment_hash.to_string())
                .collect::<HashSet<_>>(),
        ),
        Err(e) => {
            warn!(
                "Unable to list pending barkd receives, falling back to per-invoice checks: {e:#}"
            );
            None
        }
    };

    for invoice in invoices {
        if invoice_has_expired(&invoice) {
            let mut conn = state.db_pool.get()?;
            if invoice.mark_cancelled(&mut conn)? {
                info!("Cancelled expired invoice {}", invoice.id);
            }
            continue;
        }

        let payment_hash = invoice_payment_hash(&invoice);
        if bark_pending_hashes
            .as_ref()
            .is_some_and(|pending_hashes| pending_hashes.contains(&payment_hash))
        {
            continue;
        }

        if let Err(e) = claim_invoice_if_paid(state, invoice, payment_hash).await {
            error!("Unable to claim invoice: {e:#}");
        }
    }

    Ok(())
}

fn invoice_has_expired(invoice: &Invoice) -> bool {
    invoice
        .expires_at
        .is_some_and(|expires_at| expires_at <= chrono::Utc::now().naive_utc())
        || invoice.bolt11().is_expired()
}

fn invoice_payment_hash(invoice: &Invoice) -> String {
    match invoice.payment_hash.as_deref() {
        Some(payment_hash) => payment_hash.to_string(),
        None => invoice.bolt11().payment_hash().to_string(),
    }
}

async fn claim_invoice_if_paid(
    state: &State,
    invoice: Invoice,
    payment_hash: String,
) -> anyhow::Result<()> {
    let receive = state
        .barkd
        .receive_status(&payment_hash)
        .await
        .with_context(|| {
            format!(
                "failed to get lightning receive status for invoice {}",
                invoice.id
            )
        })?;

    let Some(receive) = receive else {
        warn!("Barkd has no receive status for invoice {}", invoice.id);
        return Ok(());
    };

    if receive.preimage_revealed_at.is_some() {
        let mut conn = state.db_pool.get()?;
        if invoice.mark_settled(&mut conn, receive.payment_preimage.to_string())? {
            info!("Claimed and delivered invoice {}", invoice.id);
        }
        return Ok(());
    }

    if receive.finished_at.is_some() {
        let mut conn = state.db_pool.get()?;
        if invoice.mark_cancelled(&mut conn)? {
            info!("Cancelled terminal unpaid invoice {}", invoice.id);
        }
    }

    Ok(())
}

#[cfg(test)]
mod db_tests {
    use super::*;
    use crate::models::invoice::NewInvoice;
    use crate::models::user::NewUser;
    use chrono::Duration as ChronoDuration;
    use diesel::connection::SimpleConnection;
    use diesel::prelude::*;
    use diesel::sql_types::{BigInt, Text};
    use std::env;

    struct TestSchema {
        database_url: String,
        schema: String,
    }

    impl TestSchema {
        fn new(test_name: &str) -> anyhow::Result<Option<(Self, PgConnection)>> {
            dotenv::dotenv().ok();

            let Ok(database_url) = env::var("LNURL_TEST_DATABASE_URL") else {
                eprintln!("skipping {test_name}: LNURL_TEST_DATABASE_URL is not set");
                return Ok(None);
            };

            let schema = format!(
                "lnurl_bark_test_{}_{}_{}",
                test_name,
                std::process::id(),
                chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
            );
            let schema = schema.replace('-', "_");

            let mut conn = PgConnection::establish(&database_url)?;
            conn.batch_execute(&format!(
                r#"CREATE SCHEMA "{schema}"; SET search_path TO "{schema}";"#
            ))?;

            Ok(Some((
                Self {
                    database_url,
                    schema,
                },
                conn,
            )))
        }

        fn set_search_path(&self, conn: &mut PgConnection) -> anyhow::Result<()> {
            conn.batch_execute(&format!(r#"SET search_path TO "{}";"#, self.schema))?;
            Ok(())
        }
    }

    impl Drop for TestSchema {
        fn drop(&mut self) {
            if let Ok(mut conn) = PgConnection::establish(&self.database_url) {
                let _ = conn.batch_execute(&format!(
                    r#"DROP SCHEMA IF EXISTS "{}" CASCADE;"#,
                    self.schema
                ));
            }
        }
    }

    #[derive(QueryableByName)]
    struct Count {
        #[diesel(sql_type = BigInt)]
        count: i64,
    }

    #[derive(QueryableByName)]
    struct ColumnName {
        #[diesel(sql_type = Text)]
        column_name: String,
    }

    #[test]
    fn embedded_migrations_apply_and_revert() -> anyhow::Result<()> {
        let Some((schema, mut conn)) = TestSchema::new("migrations")? else {
            return Ok(());
        };

        conn.run_pending_migrations(MIGRATIONS)
            .map_err(|e| anyhow::anyhow!("failed to run migrations: {e}"))?;

        let invoice_table_count: Count = diesel::sql_query(
            "SELECT COUNT(*) AS count FROM information_schema.tables \
             WHERE table_schema = current_schema() AND table_name = 'invoice'",
        )
        .get_result(&mut conn)?;
        assert_eq!(invoice_table_count.count, 1);

        let columns: Vec<ColumnName> = diesel::sql_query(
            "SELECT column_name FROM information_schema.columns \
             WHERE table_schema = current_schema() AND table_name = 'invoice'",
        )
        .load(&mut conn)?;
        let columns = columns
            .into_iter()
            .map(|column| column.column_name)
            .collect::<Vec<_>>();

        for expected in ["payment_hash", "created_at", "expires_at", "settled_at"] {
            assert!(
                columns.iter().any(|column| column == expected),
                "missing invoice.{expected} column"
            );
        }

        conn.revert_all_migrations(MIGRATIONS)
            .map_err(|e| anyhow::anyhow!("failed to revert migrations: {e}"))?;
        schema.set_search_path(&mut conn)?;

        let invoice_table_count: Count = diesel::sql_query(
            "SELECT COUNT(*) AS count FROM information_schema.tables \
             WHERE table_schema = current_schema() AND table_name = 'invoice'",
        )
        .get_result(&mut conn)?;
        assert_eq!(invoice_table_count.count, 0);

        Ok(())
    }

    #[test]
    fn invoice_model_uses_claim_columns() -> anyhow::Result<()> {
        let Some((_schema, mut conn)) = TestSchema::new("invoice_model")? else {
            return Ok(());
        };

        conn.run_pending_migrations(MIGRATIONS)
            .map_err(|e| anyhow::anyhow!("failed to run migrations: {e}"))?;

        let user = NewUser {
            ark_address: "ark-test-address".to_string(),
            name: "alice".to_string(),
        }
        .insert(&mut conn)?;

        let expired_invoice = NewInvoice {
            user_id: user.id,
            bolt11: "lnbc1expired".to_string(),
            amount_msats: 1_000,
            payment_hash: Some("00".repeat(32)),
            preimage: String::new(),
            lnurlp_comment: None,
            state: InvoiceState::Pending as i32,
            expires_at: Some((chrono::Utc::now() - ChronoDuration::minutes(1)).naive_utc()),
        }
        .insert(&mut conn)?;

        let active_invoice = NewInvoice {
            user_id: user.id,
            bolt11: "lnbc1active".to_string(),
            amount_msats: 2_000,
            payment_hash: Some("11".repeat(32)),
            preimage: String::new(),
            lnurlp_comment: Some("hi".to_string()),
            state: InvoiceState::Pending as i32,
            expires_at: Some((chrono::Utc::now() + ChronoDuration::minutes(5)).naive_utc()),
        }
        .insert(&mut conn)?;

        assert!(expired_invoice.created_at <= chrono::Utc::now().naive_utc());
        assert_eq!(Invoice::cancel_expired_pending(&mut conn)?, 1);

        let expired_invoice = Invoice::get_by_id(&mut conn, expired_invoice.id)?.unwrap();
        assert_eq!(expired_invoice.state, InvoiceState::Cancelled as i32);

        assert!(active_invoice.mark_settled(&mut conn, "22".repeat(32))?);
        let active_invoice = Invoice::get_by_id(&mut conn, active_invoice.id)?.unwrap();
        assert_eq!(active_invoice.state, InvoiceState::Settled as i32);
        assert_eq!(active_invoice.preimage, "22".repeat(32));
        assert!(active_invoice.settled_at.is_some());

        Ok(())
    }
}
