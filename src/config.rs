use bark::Config as BarkConfig;
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

    /// The domain name you are running lnurl-server on
    #[clap(default_value_t = String::from("localhost:3000"), long, env = "LNURL_DOMAIN")]
    pub domain: String,

    /// BIP39 mnemonic for the Bark wallet that receives Lightning payments
    #[clap(long, env = "LNURL_BARK_MNEMONIC")]
    pub bark_mnemonic: String,

    /// SQLite database path for Bark wallet state
    #[clap(default_value_t = String::from("./bark.sqlite"), long, env = "LNURL_BARK_DB_PATH")]
    pub bark_db_path: String,

    /// Ark server URL used by the Bark wallet
    #[clap(long, env = "LNURL_BARK_SERVER")]
    pub bark_server: String,

    /// Esplora URL used by the Bark wallet
    #[clap(long, env = "LNURL_BARK_ESPLORA")]
    pub bark_esplora: Option<String>,
}

impl Config {
    pub fn bark_config(&self) -> BarkConfig {
        BarkConfig {
            server_address: self.bark_server.clone(),
            esplora_address: self.bark_esplora.clone(),
            ..BarkConfig::network_default(self.network)
        }
    }
}
