use bitcoin::Network;
use clap::Parser;

#[derive(Parser, Debug, Clone)]
#[command(version, author, about)]
/// A simple LNURL pay server. Allows you to have a lightning address for your own node.
pub struct Config {
    /// Postgres connection string (e.g. postgres://user:password@localhost/dbname)
    #[clap(long, env = "LNURL_PG_URL")]
    pub pg_url: String,

    /// Nostr nsec used for zaps
    #[clap(long, env = "LNURL_NSEC")]
    pub nsec: String,

    /// Bind address for lnurl-server's webserver
    #[clap(default_value_t = String::from("0.0.0.0"), long, env = "LNURL_BIND")]
    pub bind: String,

    /// Port for lnurl-server's webserver
    #[clap(default_value_t = 3000, long, env = "LNURL_PORT")]
    pub port: u16,

    /// Bitcoin network Bark is running on ["bitcoin", "testnet", "signet, "regtest"]
    #[clap(default_value_t = Network::Bitcoin, short, long, env = "LNURL_NETWORK")]
    pub network: Network,

    /// Minimum amount in millisatoshis that can be sent via LNURL
    #[clap(default_value_t = 1_000, long, env = "LNURL_MIN_SENDABLE")]
    pub min_sendable: u64,

    /// Maximum amount in millisatoshis that can be sent via LNURL
    #[clap(default_value_t = 11_000_000_000, long, env = "LNURL_MAX_SENDABLE")]
    pub max_sendable: u64,

    /// Maximum requests accepted from each source IP per minute
    #[clap(default_value_t = 120, long, env = "LNURL_RATE_LIMIT_PER_MINUTE")]
    pub rate_limit_per_minute: u32,

    /// Maximum HTTP request body size in bytes
    #[clap(default_value_t = 16_384, long, env = "LNURL_MAX_REQUEST_BODY_BYTES")]
    pub max_request_body_bytes: usize,

    /// Maximum time spent handling an HTTP request in seconds
    #[clap(default_value_t = 10, long, env = "LNURL_REQUEST_TIMEOUT_SECONDS")]
    pub request_timeout_seconds: u64,

    /// The domain name you are running lnurl-server on
    #[clap(default_value_t = String::from("localhost:3000"), long, env = "LNURL_DOMAIN")]
    pub domain: String,

    /// Base URL for the barkd REST API
    #[clap(long, env = "LNURL_BARKD_URL")]
    pub barkd_url: String,

    /// Bearer token for the barkd REST API
    #[clap(long, env = "LNURL_BARKD_TOKEN")]
    pub barkd_token: Option<String>,
}
