use anyhow::Context;
use ark::lightning::PaymentHash;
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
        Invoice::get_by_state(&mut conn, InvoiceState::Pending as i32)?
    };

    for invoice in invoices {
        if let Err(e) = claim_invoice_if_paid(state, invoice).await {
            error!("Unable to claim invoice: {e:#}");
        }
    }

    Ok(())
}

async fn claim_invoice_if_paid(state: &State, invoice: Invoice) -> anyhow::Result<()> {
    let bolt11 = invoice.bolt11();
    let payment_hash = PaymentHash::from(&bolt11);

    let receive = state
        .barkd
        .receive_status(&payment_hash.to_string())
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

    if receive.finished_at.is_none() {
        return Ok(());
    }

    {
        let mut conn = state.db_pool.get()?;
        invoice.set_state(&mut conn, InvoiceState::Settled as i32)?;
    }

    info!("Claimed and delivered invoice {}", invoice.id);

    Ok(())
}
