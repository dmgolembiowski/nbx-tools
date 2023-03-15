pub mod markets;
pub mod accounts;

use base64::{engine::general_purpose, Engine as _};
use hmac::{Hmac, Mac};
use reqwest::{header::{HeaderMap, HeaderValue}, Method, Response};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::{
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};
use tokio::sync::RwLock;

use crate::{config::Config, error::error_for_status, Error};

fn current_time_millis() -> u128 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_millis()
}

fn calculate_hmac(data: &[u8], secret: &[u8]) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).unwrap();
    mac.update(data);
    let result = mac.finalize();
    result.into_bytes().to_vec()
}

fn generate_signature(secret: &[u8], method: Method, path: &str, body: &str) -> (String, u128) {
    let timestamp = current_time_millis();
    let data = format!("{timestamp}{method}{path}{body}");
    let signature = calculate_hmac(data.as_bytes(), secret);
    let signature: String = general_purpose::STANDARD.encode(signature);
    (signature, timestamp)
}

pub type Result<T> = std::result::Result<T, Error>;

const TOKEN_EXPIRY_MINUTES: u64 = 30;
const NBX_BASE_URL: &str = "https://api.nbx.com";

struct Token {
    token: String,
    created_at: Instant,
}

impl Default for Token {
    fn default() -> Self {
        // create an expired token
        Self {
            token: String::new(),
            created_at: Instant::now() - Duration::from_secs(TOKEN_EXPIRY_MINUTES * 60 + 10),
        }
    }
}

impl Token {
    fn is_valid(&self) -> bool {
        self.created_at.elapsed().as_secs() < TOKEN_EXPIRY_MINUTES * 60 - 10
    }
}

#[derive(Serialize)]
struct TokenRequest {
    #[serde(rename = "expiresIn")]
    expires_in: u64,
}

impl Default for TokenRequest {
    fn default() -> Self {
        Self {
            expires_in: TOKEN_EXPIRY_MINUTES,
        }
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    token: String,
}

#[derive(Clone)]
pub struct Client {
    client: reqwest::Client,
    config: Arc<Config>,
    token: Arc<RwLock<Token>>,
}

impl Client {
    pub fn new(config: Config) -> Self {
        Self {
            client: reqwest::Client::new(),
            config: Arc::new(config),
            token: Default::default(),
        }
    }

    async fn update_token(&self) -> Result<String> {
        let mut token = self.token.write().await;
        if token.is_valid() {
            // was likely updated by a different task
            Ok(token.token.clone())
        } else {
            let path = format!(
                "/accounts/{}/api_keys/{}/tokens",
                self.config.account_id, self.config.api_key
            );
            let body = TokenRequest::default();
            let body = serde_json::to_string(&body).unwrap();
            let method = Method::POST;
            let secret = general_purpose::STANDARD
                .decode(&self.config.api_secret)
                .unwrap();
            let (signature, timestamp) = generate_signature(&secret, method, &path, &body);
            let mut headers = HeaderMap::new();
            let auth = format!(
                "NBX-HMAC-SHA256 {}:{}",
                self.config.api_passphrase, signature
            );
            let timestamp: HeaderValue = format!("{}", timestamp).parse().unwrap();
            headers.insert("authorization", auth.parse().unwrap());
            headers.insert("X-NBX-TIMESTAMP", timestamp);
            headers.insert("Content-Type", "application/json".parse().unwrap());
            let newtoken: TokenResponse = error_for_status(
                self.client
                    .post(&format!("{}{}", NBX_BASE_URL, path))
                    .headers(headers)
                    .body(body)
                    .send()
                    .await?,
            )
                .await?
                .json()
                .await?;
            token.token = newtoken.token.clone();
            token.created_at = Instant::now();
            Ok(newtoken.token)
        }
    }

    async fn bearer(&self) -> Result<String> {
        let token = self.token.read().await;
        let token = if token.is_valid() {
            token.token.clone()
        } else {
            drop(token);
            self.update_token().await?
        };
        Ok(token)
    }

    pub(crate) async fn get(&self, path: &str) -> Result<Response> {
        let bearer = self.bearer().await?;
        error_for_status(
            self.client.get(format_path(path))
                .bearer_auth(bearer)
                .send()
                .await?
        ).await
    }

    pub(crate) async fn get_query<Q>(&self, path: &str, query: &Q) -> Result<Response>
    where
        Q: Serialize + ?Sized,
    {
        let bearer = self.bearer().await?;
        error_for_status(
            self.client
                .get(format_path(path))
                .bearer_auth(bearer)
                .query(query)
                .send()
                .await?,
        )
        .await
    }

    pub(crate) async fn post<D>(&self, path: &str, data: &D) -> Result<Response>
    where
        D: Serialize,
    {
        let bearer = self.bearer().await?;
        error_for_status(
            self.client
                .post(format_path(path))
                .bearer_auth(bearer)
                .json(data)
                .send()
                .await?,
        )
        .await
    }

    pub(crate) async fn delete(&self, path: &str) -> Result<Response> {
        let bearer = self.bearer().await?;
        error_for_status(
            self.client.delete(format_path(path))
                .bearer_auth(bearer)
                .send()
                .await?
        ).await
    }
}

fn format_path(path: &str) -> String {
    if path.starts_with("https://") {
        path.to_string()
    } else {
        format!("{}{}", NBX_BASE_URL, path)
    }
}
