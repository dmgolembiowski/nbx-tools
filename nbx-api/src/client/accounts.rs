use serde::{Deserialize, Serialize};

use crate::error::error_for_status;

use super::{Client, Result};

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
#[serde(rename_all = "UPPERCASE")]
pub enum Side {
    Buy,
    Sell,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
#[serde(rename_all = "UPPERCASE")]
pub enum Type {
    Limit,
    Market,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub enum TimeInForceType {
    #[serde(rename = "GOOD_TIL_CANCELED", alias = "GOOD-TIL-CANCELED")]
    GoodTilCanceled,
    #[serde(rename = "IMMEDIATE_OR_CANCEL", alias = "IMMEDIATE_OR_CANCEL")]
    ImmediateOrCancel,
}

#[derive(Serialize, Deserialize, Debug)]
struct TimeInForce {
    #[serde(rename = "type")]
    typ: TimeInForceType,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "UPPERCASE")]
enum FreezeType {
    Amount,
}

#[derive(Serialize, Deserialize, Debug)]
struct Freeze {
    #[serde(rename = "type")]
    typ: FreezeType,
    value: String,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Execution {
    #[serde(rename = "type")]
    pub typ: Type,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price: Option<String>,
    time_in_force: TimeInForce,
    #[serde(skip_serializing_if = "Option::is_none")]
    freeze: Option<Freeze>,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct OrderRequest {
    market: String,
    quantity: String,
    side: Side,
    execution: Execution,
}

pub struct CreateOrder(String);

impl CreateOrder {
    pub fn new(market: &str) -> Self {
        Self(market.to_string())
    }
    pub fn sell(self, quantity: &str) -> CreateSellOrder {
        CreateSellOrder {
            market: self.0,
            quantity: quantity.to_string(),
        }
    }
    pub fn buy(self, quantity: &str) -> CreateBuyOrder {
        CreateBuyOrder {
            market: self.0,
            quantity: quantity.to_string(),
        }
    }
}

pub struct CreateSellOrder {
    market: String,
    quantity: String,
}

impl CreateSellOrder {
    pub fn market(self) -> OrderRequest {
        OrderRequest {
            market: self.market,
            quantity: self.quantity,
            side: Side::Sell,
            execution: Execution {
                typ: Type::Market,
                time_in_force: TimeInForce {
                    typ: TimeInForceType::ImmediateOrCancel,
                },
                price: None,
                freeze: None,
            },
        }
    }

    pub fn limit(self, price: &str) -> OrderRequest {
        OrderRequest {
            market: self.market,
            quantity: self.quantity,
            side: Side::Sell,
            execution: Execution {
                typ: Type::Limit,
                time_in_force: TimeInForce {
                    typ: TimeInForceType::GoodTilCanceled,
                },
                price: Some(price.to_string()),
                freeze: None,
            },
        }
    }
}

pub struct CreateBuyOrder {
    market: String,
    quantity: String,
}

impl CreateBuyOrder {
    pub fn market(self) -> OrderRequest {
        OrderRequest {
            market: self.market,
            quantity: self.quantity,
            side: Side::Buy,
            execution: Execution {
                typ: Type::Market,
                time_in_force: TimeInForce {
                    typ: TimeInForceType::ImmediateOrCancel,
                },
                price: None,
                freeze: Some(todo!()),
            },
        }
    }

    pub fn limit(self, price: &str) -> OrderRequest {
        OrderRequest {
            market: self.market,
            quantity: self.quantity,
            side: Side::Buy,
            execution: Execution {
                typ: Type::Limit,
                time_in_force: TimeInForce {
                    typ: TimeInForceType::GoodTilCanceled,
                },
                price: Some(price.to_string()),
                freeze: None,
            },
        }
    }
}

impl OrderRequest {
    pub fn time_in_force(mut self, time_in_force: TimeInForceType) -> OrderRequest {
        self.execution.time_in_force.typ = time_in_force;
        self
    }
}

impl Client {
    pub async fn create_order(&self, account_id: &str, request: &OrderRequest) -> Result<()> {
        error_for_status(
            self.post(&format!("/accounts/{}/orders", account_id), request)
                .await?,
        )
        .await?;
        Ok(())
    }

    pub async fn cancel_order(&self, market: &str, order_id: &str) -> Result<()> {
        error_for_status(
            self.delete(&format!("/markets/{}/orders/{}", market, order_id))
                .await?,
        )
        .await?;
        Ok(())
    }

    pub async fn accounts_open_orders_by_market(
        &self,
        account_id: &str,
        market: &str,
    ) -> Result<OpenOrdersResponse> {
        let resp = error_for_status(
            self.get(&format!(
                "/accounts/{}/markets/{}/orders",
                account_id, market
            ))
            .await?,
        )
        .await?;
        let next_page_url = resp
            .headers()
            .get("x-next-page-url")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let orders: Vec<MarketOrder> = resp.json().await?;
        let orders = orders
            .into_iter()
            .map(|o| o.into_order(market))
            .collect::<Vec<_>>();
        Ok(OpenOrdersResponse {
            orders,
            next_page_url,
            client: self.clone(),
            market: Some(market.to_string()),
        })
    }

    pub async fn accounts_orders(&self, account_id: &str) -> Result<OpenOrdersResponse> {
        let resp = error_for_status(
            self.get(&format!("/accounts/{}/orders", account_id,))
                .await?,
        )
        .await?;
        let next_page_url = resp
            .headers()
            .get("x-next-page-url")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let orders: Vec<Order> = resp.json().await?;
        Ok(OpenOrdersResponse {
            orders,
            next_page_url,
            client: self.clone(),
            market: None,
        })
    }

    pub async fn accounts_asset(&self, account_id: &str, asset_id: &str) -> Result<Asset> {
        Ok(error_for_status(
            self.get(&format!("/accounts/{}/assets/{}", account_id, asset_id))
                .await?,
        )
        .await?
        .json()
        .await?)
    }
}

pub struct OpenOrdersResponse {
    pub orders: Vec<Order>,
    next_page_url: Option<String>,
    client: Client,
    market: Option<String>,
}

impl OpenOrdersResponse {
    pub async fn next_page(self) -> Result<Option<OpenOrdersResponse>> {
        if let Some(next_page_url) = self.next_page_url {
            let resp = error_for_status(self.client.get(&next_page_url).await?).await?;
            let next_page_url = resp
                .headers()
                .get("x-next-page-url")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());
            let orders = if let Some(market) = &self.market {
                let orders: Vec<MarketOrder> = resp.json().await?;
                orders
                    .into_iter()
                    .map(|o| o.into_order(market))
                    .collect::<Vec<_>>()
            } else {
                let orders: Vec<Order> = resp.json().await?;
                orders
            };
            Ok(Some(OpenOrdersResponse {
                orders,
                next_page_url,
                client: self.client,
                market: self.market,
            }))
        } else {
            Ok(None)
        }
    }
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct MarketOrder {
    pub id: String,
    pub quantity: String,
    pub side: Side,
    pub execution: Execution,
    pub events: Events,
    pub fills: Vec<Fill>,
}

impl MarketOrder {
    fn into_order(self, market: &str) -> Order {
        Order {
            id: self.id,
            market: market.to_string(),
            quantity: self.quantity,
            side: self.side,
            execution: self.execution,
            events: self.events,
            fills: self.fills,
        }
    }
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Order {
    pub id: String,
    pub market: String,
    pub quantity: String,
    pub side: Side,
    pub execution: Execution,
    pub events: Events,
    pub fills: Vec<Fill>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Events {
    pub created_at: String,
    pub opened_at: Option<String>,
    pub closed_at: Option<String>,
    pub rejected_at: Option<String>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Fill {
    pub quantity: String,
    pub price: String,
    pub fee: String,
    pub created_at: String,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Asset {
    pub id: String,
    pub balance: Balance,
    pub freezes: Vec<AssetFreeze>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Balance {
    pub available: String,
    pub total: String,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AssetFreeze {
    pub id: String,
    pub amount: String,
}
