use std::fmt::Display;

use futures::{StreamExt, SinkExt};
use serde::Deserialize;
use tokio::net::TcpStream;
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};

use crate::client::markets::{TradeOrder, Order};

#[derive(Debug)]
pub enum WebSocketError {
    WebSocketError(tokio_tungstenite::tungstenite::Error),
    SerdeJsonError {
        error: serde_json::Error,
        raw: String,
    },
}

impl From<tokio_tungstenite::tungstenite::Error> for WebSocketError {
    fn from(value: tokio_tungstenite::tungstenite::Error) -> Self {
        WebSocketError::WebSocketError(value)
    }
}

impl Display for WebSocketError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WebSocketError(error) => {
                write!(f, "WebSocket Error: {}", error)
            }
            Self::SerdeJsonError { error, raw } => {
                write!(f, "Error parsing json: {}\n{}", raw, error)
            }
        }
    }
}

impl std::error::Error for WebSocketError {}

pub struct WebSocket {
    stream: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

impl WebSocket {
    pub async fn connect(market: &str) -> Result<WebSocket, WebSocketError> {
        let (stream, _) =
            connect_async(&format!("wss://api.nbx.com/markets/{}/events", market)).await?;
        Ok(WebSocket { stream })
    }

    pub async fn next(&mut self) -> Result<Option<WebSocketEvent>, WebSocketError> {
        loop {
            match self.stream.next().await.transpose()? {
                Some(Message::Text(data)) => {
                    let event: WebSocketEvent = serde_json::from_str(&data).map_err(|error| {
                        WebSocketError::SerdeJsonError {
                            error,
                            raw: data.to_string(),
                        }
                    })?;
                    return Ok(Some(event));
                }
                Some(Message::Ping(data)) => {
                    self.stream.send(Message::Pong(data)).await.ok();
                }
                Some(_) => {} // ignoring non-text messages
                None => return Ok(None),
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum WebSocketEvent {
    #[serde(rename = "TRADE-CREATED")]
    TradeCreated(Trade),
    #[serde(rename = "ORDER-OPENED")]
    OrderOpened(Order),
    #[serde(rename = "ORDER-CLOSED")]
    OrderClosed(Order),
}

#[derive(Deserialize)]
pub struct Trade {
    pub price: String,
    pub quantity: String,
    pub timestamp: String,
    pub maker: TradeOrder,
    pub taker: TradeOrder,
}
