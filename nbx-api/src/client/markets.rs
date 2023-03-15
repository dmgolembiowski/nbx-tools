use serde::Deserialize;

use crate::error::error_for_status;

use super::{Client, Result, accounts::Side};

#[derive(Deserialize, Debug)]
pub struct TradeOrder {
    pub id: String,
    pub side: Side,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Trade {
    pub maker_order: TradeOrder,
    pub taker_order: TradeOrder,
    pub price: String,
    pub quantity: String,
    pub created_at: String,
}

pub struct TradesResult {
    pub trades: Vec<Trade>,
    next_page_url: Option<String>,
    client: Client,
}

impl TradesResult {
    pub async fn next_page(self) -> Result<Option<TradesResult>> {
        if let Some(next_page_url) = self.next_page_url {
            let resp = error_for_status(self.client.get(&next_page_url).await?).await?;
            let next_page_url = resp
                .headers()
                .get("x-next-page-url")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());
            let trades: Vec<Trade> = resp.json().await?;
            Ok(Some(TradesResult {
                trades,
                next_page_url,
                client: self.client,
            }))
        } else {
            Ok(None)
        }
    }
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Market {
    pub name: String,
    pub id: String,
    pub disabled: bool,
    pub base_asset: String,
    pub status: String,
    pub cancel_only: bool,
    pub limit_only: bool,
    pub quote_asset: String,
    pub status_description: Option<String>,
    pub quote_increment: String,
    pub base_size_minimum: String,
    pub base_increment: String,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Order {
    pub id: String,
    pub side: Side,
    pub price: String,
    pub quantity: String,
    pub timestamp: Option<String>, // needed for websockets
}

pub struct OrdersResult {
    pub orders: Vec<Order>,
    next_page_url: Option<String>,
    client: Client,
}

impl OrdersResult {
    pub async fn next_page(self) -> Result<Option<OrdersResult>> {
        if let Some(next_page_url) = self.next_page_url {
            let resp = error_for_status(self.client.get(&next_page_url).await?).await?;
            let next_page_url = resp
                .headers()
                .get("x-next-page-url")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());
            let orders: Vec<Order> = resp.json().await?;
            Ok(Some(OrdersResult {
                orders,
                next_page_url,
                client: self.client,
            }))
        } else {
            Ok(None)
        }
    }
}

impl Client {
    pub async fn markets_trades(&self, market: &str) -> Result<TradesResult> {
        let resp =
            error_for_status(self.get(&format!("/markets/{}/trades", market)).await?).await?;
        let next_page_url = resp
            .headers()
            .get("x-next-page-url")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let trades: Vec<Trade> = resp.json().await?;
        Ok(TradesResult {
            trades,
            next_page_url,
            client: self.clone(),
        })
    }

    pub async fn markets_orders(&self, market: &str, side: Option<Side>) -> Result<OrdersResult> {
        let mut query = vec![];
        if let Some(side) = side {
            query.push(("side", side));
        }
        let resp =
            error_for_status(self.get_query(&format!("/markets/{}/orders", market), &query).await?).await?;
        let next_page_url = resp
            .headers()
            .get("x-next-page-url")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let orders: Vec<Order> = resp.json().await?;
        Ok(OrdersResult {
            orders,
            next_page_url,
            client: self.clone(),
        })
    }

    pub async fn market(&self, market: &str) -> Result<Market> {
        Ok(
            error_for_status(self.get(&format!("/markets/{}", market)).await?)
                .await?
                .json()
                .await?,
        )
    }
}
