use serde::Deserialize;

#[derive(Deserialize, Clone)]
pub struct Config {
    pub account_id: String,
    pub api_key: String,
    pub api_secret: String,
    pub api_passphrase: String,
}
