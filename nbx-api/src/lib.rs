mod client;
mod config;
mod error;
mod websocket;

pub use client::Client;
pub use config::Config;
pub use error::Error;
pub use client::markets::{self, Order};
pub use client::accounts::{self, CreateOrder, Side};
pub use websocket::{WebSocket, WebSocketError, WebSocketEvent};
