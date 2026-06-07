use anyhow::Context;
use axum::extract::DefaultBodyLimit;
use axum::http::Method;
use axum::routing::{get, post};
use axum::{http, Extension, Router};
use bark::ark::lightning::PaymentHash;
use bark::bip39::Mnemonic;
use bark::lock_manager::memory::MemoryLockManager;
use bark::persist::sqlite::SqliteClient;
use bark::Wallet;
use clap::Parser;
use diesel::r2d2::{ConnectionManager, Pool};
use diesel::PgConnection;
use log::{error, info, warn};
use nostr::Keys;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tower_http::cors::{Any, CorsLayer};

use crate::config::*;
use crate::models::invoice::{Invoice, InvoiceState};
use crate::routes::*;

mod config;
mod models;
mod routes;

#[derive(Clone)]
pub struct State {
    pub db_pool: Pool<ConnectionManager<PgConnection>>,
    pub keys: Keys,
    pub wallet: Arc<Wallet>,

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

    let bark_mnemonic = Mnemonic::from_str(&config.bark_mnemonic)?;
    let bark_config = config.bark_config();
    let bark_db = Arc::new(SqliteClient::open(&config.bark_db_path)?);
    let wallet = match Wallet::open(
        &bark_mnemonic,
        bark_db.clone(),
        bark_config.clone(),
        Box::new(MemoryLockManager::new()),
    )
    .await
    {
        Ok(wallet) => wallet,
        Err(e) => {
            warn!("Unable to open Bark wallet, creating a new one: {e}");
            Wallet::create(
                &bark_mnemonic,
                config.network,
                bark_config,
                bark_db,
                Box::new(MemoryLockManager::new()),
                false,
            )
            .await?
        }
    };
    let wallet = Arc::new(wallet);

    let state = State {
        db_pool: db_pool.clone(),
        keys: keys.clone(),
        wallet,
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
        .layer(DefaultBodyLimit::max(1_000_000)); // max 1mb body size

    let server = axum::Server::bind(&addr).serve(server_router.into_make_service());

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
        .wallet
        .try_claim_lightning_receive(payment_hash, false, None)
        .await
        .with_context(|| {
            format!(
                "failed to claim lightning receive for invoice {}",
                invoice.id
            )
        })?;

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
