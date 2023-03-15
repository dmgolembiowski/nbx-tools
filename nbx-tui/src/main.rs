#![allow(clippy::too_many_arguments)]
use std::{
    cmp::Ordering,
    collections::HashMap,
    fs, io,
    ops::Range,
    sync::{
        atomic::{self, AtomicBool},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use chrono::TimeZone;
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use nbx_api::{
    accounts::{self, Type},
    markets, Client, Config, CreateOrder, Order, Side, WebSocket, WebSocketEvent,
};
use rand::Rng;
use serde::Deserialize;
use tokio::{
    runtime,
    sync::mpsc::{channel, Sender},
};
use tui::{
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Corner, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Span, Spans},
    widgets::{Block, Borders, Cell, List, ListItem, Paragraph, Row, Table, TableState},
    Frame, Terminal,
};

const MARKET: &str = "ETH-NOK";
const ASSETS: &[&str; 2] = &["ETH", "NOK"];
const TARGET_ASSET: &str = "NOK";
const TARGET_VALUE: f64 = 50_000.0;
const DEFAULT_MIN_SELL_PRICE: f64 = 18000.0;
const SELL_POLL_DELAY: Duration = Duration::from_secs(5);
const BASE_ORDER_DELAY: Duration = Duration::from_secs(30 * 60);
const ORDER_DELAY_VARIANCE_SECONDS: u64 = 5 * 60;
const ORDER_PRICE_INCREASE_TRIGGER: f64 = 100.0;
const COINBASE_POLL_DELAY: Duration = Duration::from_secs(10);
const COINBASE_PRICE_HISTORY_MAX_TIME: Duration = Duration::from_secs(60 * 60 * 24);
const ERROR_DELAY: Duration = Duration::from_secs(5 * 60);
const WEBSOCKET_RESET_TIME: Duration = Duration::from_secs(60 * 60);
const QUANTITY_RANGE: Range<f64> = 0.1..0.4;

type FillsData = (chrono::DateTime<chrono::Local>, accounts::Type, f64, String);

#[derive(Default)]
struct PriceHistory(Vec<(Instant, f64)>);

impl PriceHistory {
    fn add(&mut self, price: f64) {
        self.0.push((Instant::now(), price));
    }

    fn trim(&mut self, duration: Duration) {
        let now = Instant::now();
        if let Some(idx) = self.0.iter().position(|(i, _)| now - *i > duration) {
            if idx > 0 {
                self.0.drain(0..idx);
            }
        }
    }

    fn stats(&self, duration: Duration) -> (f64, f64, f64) {
        let now = Instant::now();
        let (total, impressions, min, max) = self
            .0
            .iter()
            .rev()
            .take_while(|(i, _)| now - *i < duration)
            .fold(
                (0.0, 0usize, 100000000.0f64, 0.0f64),
                |(total, impressions, min, max), (_, price)| {
                    (
                        total + price,
                        impressions + 1,
                        min.min(*price),
                        max.max(*price),
                    )
                },
            );
        (total / impressions as f64, min, max)
    }

    fn last(&self) -> Option<f64> {
        self.0.iter().last().map(|(_, p)| *p)
    }
}

enum OrderingStatus {
    Waiting(chrono::DateTime<chrono::Local>),
    Trying,
    Error(String),
}

fn main() -> anyhow::Result<()> {
    let rt = runtime::Builder::new_multi_thread()
        .enable_time()
        .enable_io()
        .build()
        .unwrap();

    let config = fs::read_to_string("config.toml")?;
    let config: Config = toml::from_str(&config)?;
    let account_id = config.account_id.clone();
    let nbxclient = Client::new(config);
    let has_open_orders = Arc::new(AtomicBool::new(false));
    let ignore_coinbase_price = Arc::new(AtomicBool::new(false));
    let orders = Arc::new(Mutex::new(HashMap::new()));
    let trade_history = Arc::new(Mutex::new(Vec::new()));
    let assets = Arc::new(Mutex::new(HashMap::new()));
    let min_sell_price = Arc::new(Mutex::new(DEFAULT_MIN_SELL_PRICE));
    rt.block_on({
        let trade_history = trade_history.clone();
        let has_open_orders = has_open_orders.clone();
        let nbxclient = nbxclient.clone();
        let account_id = account_id.clone();
        let assets = assets.clone();
        async move {
            let open_orders = nbxclient
                .accounts_open_orders_by_market(&account_id, MARKET)
                .await
                .unwrap();
            has_open_orders.store(!open_orders.orders.is_empty(), atomic::Ordering::Relaxed);

            tokio::spawn({
                let nbxclient = nbxclient.clone();
                let trade_history = trade_history.clone();
                let account_id = account_id.clone();
                async move {
                    let fills = fetch_account_fills(&nbxclient, &account_id, MARKET, 15)
                        .await
                        .unwrap();
                    let mut lock = trade_history.lock().unwrap();
                    *lock = fills;
                }
            });

            for asset in ASSETS {
                if let Ok(val) = nbxclient.accounts_asset(&account_id, asset).await {
                    let balance = val.balance.total.parse::<f64>().unwrap();
                    assets.lock().unwrap().insert(*asset, balance);
                }
            }
        }
    });
    let ws_error = Arc::new(Mutex::new(None));
    rt.spawn({
        let orders = orders.clone();
        let trade_history = trade_history.clone();
        let assets = assets.clone();
        let account_id = account_id.clone();
        let has_open_orders = has_open_orders.clone();
        let nbxclient = nbxclient.clone();
        async move {
            'outer: loop {
                {
                    // reset orders
                    let mut orders = orders.lock().unwrap();
                    orders.clear();
                }
                let Ok(ordersres) = nbxclient.markets_orders(MARKET, None).await else {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                };
                let mut ordersres = Some(ordersres);
                while let Some(res) = ordersres.take() {
                    {
                        let mut orders = orders.lock().unwrap();
                        for order in &res.orders {
                            let Ok(q) = order.quantity.parse::<f64>() else { continue; };
                            if q < 0.00001 {
                                continue;
                            }
                            orders.insert(order.id.clone(), order.clone());
                        }
                    }
                    ordersres = match res.next_page().await {
                        Ok(ordersres) => ordersres,
                        Err(_) => {
                            tokio::time::sleep(Duration::from_secs(5)).await;
                            continue;
                        }
                    };
                }
                let start = Instant::now();
                match WebSocket::connect(MARKET).await {
                    Ok(mut ws) => loop {
                        if start.elapsed() > WEBSOCKET_RESET_TIME {
                            continue 'outer;
                        }
                        match ws.next().await {
                            Ok(Some(WebSocketEvent::OrderOpened(order))) => {
                                let Ok(q) = order.quantity.parse::<f64>() else { continue; };
                                if q < 0.00001 {
                                    continue;
                                }
                                orders.lock().unwrap().insert(order.id.clone(), order);
                            }
                            Ok(Some(WebSocketEvent::OrderClosed(order))) => {
                                orders.lock().unwrap().remove(&order.id);
                            }
                            Ok(Some(WebSocketEvent::TradeCreated(trade))) => {
                                tokio::spawn({
                                    let trade_history = trade_history.clone();
                                    let nbxclient = nbxclient.clone();
                                    let account_id = account_id.clone();
                                    async move {
                                        if let Ok(fills) =
                                            fetch_account_fills(&nbxclient, &account_id, MARKET, 15)
                                                .await
                                        {
                                            let mut lock = trade_history.lock().unwrap();
                                            *lock = fills;
                                        }
                                    }
                                });
                                for asset in ASSETS {
                                    if let Ok(val) =
                                        nbxclient.accounts_asset(&account_id, asset).await
                                    {
                                        if let Ok(balance) = val.balance.total.parse::<f64>() {
                                            assets.lock().unwrap().insert(*asset, balance);
                                        }
                                    }
                                }
                                if has_open_orders.load(atomic::Ordering::Relaxed) {}
                                let Ok(tq) = trade.quantity.parse::<f64>() else { continue };
                                let mut lock = orders.lock().unwrap();
                                if let Some(o) = lock.get_mut(&trade.maker.id) {
                                    let Ok(oq) = o.quantity.parse::<f64>() else { continue };
                                    o.quantity = format!("{}", oq - tq);
                                }
                                if let Some(o) = lock.get_mut(&trade.taker.id) {
                                    let Ok(oq) = o.quantity.parse::<f64>() else { continue };
                                    o.quantity = format!("{}", oq - tq);
                                }
                            }
                            Ok(None) => {
                                continue 'outer;
                            }
                            Err(err) => {
                                ws_error.lock().unwrap().replace(format!("{}", err));
                                tokio::time::sleep(Duration::from_secs(5)).await;
                                continue 'outer;
                            }
                        }
                    },
                    Err(err) => {
                        ws_error.lock().unwrap().replace(format!("{}", err));
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                }
            }
        }
    });

    let price_history = Arc::new(Mutex::new(PriceHistory::default()));
    rt.spawn({
        #[derive(Deserialize, Debug)]
        struct CbSellPrice {
            data: CbSellPriceData,
        }
        #[derive(Deserialize, Debug)]
        struct CbSellPriceData {
            #[allow(unused)]
            base: String,
            #[allow(unused)]
            currency: String,
            amount: String,
        }

        async fn get_coinbase_sell_price(client: &reqwest::Client) -> anyhow::Result<f64> {
            let cbsellpriceresp: CbSellPrice = client
                .get(&format!(
                    "https://api.coinbase.com/v2/prices/{}/sell",
                    MARKET
                ))
                .send()
                .await?
                .json()
                .await?;
            let cbsellprice = cbsellpriceresp.data.amount.parse::<f64>()?;
            Ok(cbsellprice)
        }

        let client = reqwest::Client::new();
        let price_history = price_history.clone();
        async move {
            loop {
                if let Ok(cbsellprice) = get_coinbase_sell_price(&client).await {
                    let mut lock = price_history.lock().unwrap();
                    lock.add(cbsellprice);
                    lock.trim(COINBASE_PRICE_HISTORY_MAX_TIME);
                }
                tokio::time::sleep(COINBASE_POLL_DELAY).await;
            }
        }
    });

    let ordering_status = Arc::new(Mutex::new(OrderingStatus::Trying));
    let (force_order_tx, mut force_order_rx) = channel::<()>(10);
    rt.spawn({
        let price_history = price_history.clone();
        let trade_history = trade_history.clone();
        let assets = assets.clone();
        let orders = orders.clone();
        let ordering_status = ordering_status.clone();
        let min_sell_price = min_sell_price.clone();
        let ignore_coinbase_price = ignore_coinbase_price.clone();

        async move {
            let mut next_order = Instant::now() + Duration::from_secs(5 * 60);
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(SELL_POLL_DELAY) => {},
                    _ = force_order_rx.recv() => {
                        next_order = Instant::now();
                    }
                };
                let target_asset_value =
                    { *assets.lock().unwrap().get(TARGET_ASSET).unwrap_or(&0.0) };
                if target_asset_value >= TARGET_VALUE {
                    break;
                }
                let min_sell_price = { *min_sell_price.lock().unwrap() };
                let last_order_value = {
                    trade_history
                        .lock()
                        .unwrap()
                        .iter()
                        .next()
                        .and_then(|f| f.3.parse::<f64>().ok())
                        .unwrap_or(min_sell_price)
                };
                let limit = if ignore_coinbase_price.load(atomic::Ordering::Relaxed) {
                    min_sell_price
                } else {
                    let Some(lastcbprice) = price_history
                        .lock()
                        .unwrap()
                        .last() else {
                            continue;
                        };
                    min_sell_price.max(lastcbprice)
                };
                if has_open_orders.load(atomic::Ordering::Relaxed) {
                    if let Ok(open_orders) = nbxclient
                        .accounts_open_orders_by_market(&account_id, MARKET)
                        .await
                    {
                        let mut had_unfilled_orders = false;
                        let mut failed_to_cancel = false;
                        for order in open_orders.orders {
                            had_unfilled_orders = order.fills.is_empty() || had_unfilled_orders;
                            failed_to_cancel =
                                nbxclient.cancel_order(MARKET, &order.id).await.is_err()
                                    || failed_to_cancel;
                        }
                        if !failed_to_cancel && had_unfilled_orders {
                            // orders were cancelled without being filled at all, lets try again
                            next_order = Instant::now();
                        }
                        if !failed_to_cancel {
                            has_open_orders.store(false, atomic::Ordering::Relaxed);
                        }
                    }
                }
                if Instant::now() >= next_order
                    || limit >= last_order_value + ORDER_PRICE_INCREASE_TRIGGER
                {
                    let available_over_limit = {
                        orders.lock().unwrap().values().fold(0.0, |acc, o| {
                            if matches!(o.side, Side::Sell) {
                                return acc;
                            }
                            let Ok(oprice) = o.price.parse::<f64>() else {
                                return acc;
                            };
                            let Ok(oquantity) = o.quantity.parse::<f64>() else {
                                return acc;
                            };
                            if oprice >= limit {
                                acc + oquantity
                            } else {
                                acc
                            }
                        })
                    };
                    let quantity: f64 = {
                        let mut rng = rand::thread_rng();
                        rng.gen_range(QUANTITY_RANGE)
                    };
                    let (quantity, limit) = if available_over_limit > QUANTITY_RANGE.start {
                        (
                            if available_over_limit < quantity {
                                available_over_limit
                            } else {
                                quantity
                            },
                            limit,
                        )
                    } else {
                        //(quantity, limit) // limit + COINBASE_PRICE_MARKUP
                        continue;
                    };
                    let quantity = format!("{:.5}", quantity);
                    let limit = format!("{:.2}", limit);
                    //let order = CreateOrder::new(MARKET).sell(&quantity).market();
                    let order = CreateOrder::new(MARKET).sell(&quantity).limit(&limit);
                    match nbxclient.create_order(&account_id, &order).await {
                        Ok(_) => {
                            has_open_orders.store(true, atomic::Ordering::Relaxed);
                            let mut rng = rand::thread_rng();
                            let variance = rng.gen_range(0..ORDER_DELAY_VARIANCE_SECONDS);
                            let variance = Duration::from_secs(variance);
                            let plus_minus: bool = rng.gen();
                            let offset = if plus_minus {
                                BASE_ORDER_DELAY + variance
                            } else {
                                BASE_ORDER_DELAY - variance
                            };
                            next_order = Instant::now() + offset;
                            let time_next =
                                chrono::Local::now() + chrono::Duration::from_std(offset).unwrap();

                            let mut lock = ordering_status.lock().unwrap();
                            *lock = OrderingStatus::Waiting(time_next);
                        }
                        Err(err) => {
                            next_order = Instant::now() + ERROR_DELAY;
                            let mut lock = ordering_status.lock().unwrap();
                            *lock = OrderingStatus::Error(format!("{}", err));
                        }
                    }
                    // } else if Instant::now() >= next_order {
                    //     let mut lock = ordering_status.lock().unwrap();
                    //     *lock = OrderingStatus::Trying;
                    // } else {
                    //     let time_next = chrono::Local::now()
                    //         + chrono::Duration::from_std(next_order - Instant::now()).unwrap();
                    //     let mut lock = ordering_status.lock().unwrap();
                    //     *lock = OrderingStatus::Waiting(time_next);
                    // }
                };
            }
        }
    });

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let res = run_app(
        &mut terminal,
        orders,
        price_history,
        trade_history,
        assets,
        ordering_status,
        min_sell_price,
        ignore_coinbase_price,
        force_order_tx,
    );

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        println!("{:?}", err)
    }

    Ok(())
}

fn run_app<B: Backend>(
    terminal: &mut Terminal<B>,
    orders: Arc<Mutex<HashMap<String, markets::Order>>>,
    price_history: Arc<Mutex<PriceHistory>>,
    trade_history: Arc<Mutex<Vec<FillsData>>>,
    assets: Arc<Mutex<HashMap<&'static str, f64>>>,
    ordering_status: Arc<Mutex<OrderingStatus>>,
    min_sell_price: Arc<Mutex<f64>>,
    ignore_coinbase_price: Arc<AtomicBool>,
    force_order_tx: Sender<()>,
) -> io::Result<()> {
    let mut price_history_table_state = TableState::default();
    let mut trade_history_table_state = TableState::default();
    let mut asset_table_state = TableState::default();
    loop {
        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(40), Constraint::Percentage(60)].as_ref())
                .split(f.size());
            draw_orders(f, orders.clone(), min_sell_price.clone(), ignore_coinbase_price.clone(), chunks[0]);
            let chunks1 = Layout::default()
                .direction(Direction::Vertical)
                .constraints(
                    [
                        Constraint::Length(5),
                        Constraint::Length(4),
                        Constraint::Length(3),
                        Constraint::Max(100),
                    ]
                    .as_ref(),
                )
                .split(chunks[1]);
            draw_price_history(
                f,
                &mut price_history_table_state,
                price_history.clone(),
                chunks1[0],
            );
            draw_assets(f, &mut asset_table_state, assets.clone(), chunks1[1]);
            let (text, style) = {
                let lock = ordering_status.lock().unwrap();
                match &*lock {
                    OrderingStatus::Error(error) => {
                        (error.clone(), Style::default().fg(Color::Red))
                    }
                    OrderingStatus::Trying => (
                        "Trying to make order...".to_string(),
                        Style::default().fg(Color::White),
                    ),
                    OrderingStatus::Waiting(time) => (
                        format!("Next order at: {}", time.format("%Y-%m-%d %H:%M:%S")),
                        Style::default().fg(Color::White),
                    ),
                }
            };
            let status = Paragraph::new(vec![Spans::from(vec![Span::styled(&text, style)])]).block(
                Block::default()
                    .title("Next Order Status")
                    .borders(Borders::ALL),
            );
            f.render_widget(status, chunks1[2]);
            draw_trade_history(
                f,
                &mut trade_history_table_state,
                trade_history.clone(),
                chunks1[3],
            );
        })?;

        if event::poll(Duration::from_millis(200))? {
            match event::read()? {
                Event::Key(KeyEvent {
                    code: KeyCode::Esc, ..
                }) => {
                    return Ok(());
                }
                Event::Key(KeyEvent {
                    code: KeyCode::Char('q'),
                    ..
                }) => {
                    return Ok(());
                }
                Event::Key(KeyEvent {
                    code: KeyCode::Char('c'),
                    modifiers,
                    ..
                }) if modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(());
                }
                Event::Key(KeyEvent {
                    code: KeyCode::Char('B'),
                    modifiers,
                    ..
                }) if modifiers.contains(KeyModifiers::SHIFT) => {
                    force_order_tx.blocking_send(()).ok();
                }
                Event::Key(KeyEvent {
                    code: KeyCode::PageUp,
                    modifiers,
                    ..
                }) => {
                    let mut lock = min_sell_price.lock().unwrap();
                    if modifiers.contains(KeyModifiers::SHIFT) {
                        *lock += 100.0;
                    } else {
                        *lock += 10.0;
                    }
                }
                Event::Key(KeyEvent {
                    code: KeyCode::PageDown,
                    modifiers,
                    ..
                }) => {
                    let mut lock = min_sell_price.lock().unwrap();
                    if modifiers.contains(KeyModifiers::SHIFT) {
                        *lock -= 100.0;
                    } else {
                        *lock -= 10.0;
                    }
                }
                Event::Key(KeyEvent {
                    code: KeyCode::Char('i'),
                    ..
                }) => {
                    ignore_coinbase_price.fetch_xor(true, atomic::Ordering::Relaxed);
                }
                _ => {}
            }
        }
    }
}

fn draw_orders<B: Backend>(
    f: &mut Frame<B>,
    orders: Arc<Mutex<HashMap<String, Order>>>,
    min_sell_price: Arc<Mutex<f64>>,
    ignore_coinbase_price: Arc<AtomicBool>,
    area: Rect,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(
            [
                Constraint::Length(3),
                Constraint::Max(50),
                Constraint::Percentage(60),
            ]
            .as_ref(),
        )
        .split(area);
    let lock = orders.lock().unwrap();
    let mut orders = lock.values().cloned().collect::<Vec<_>>();
    drop(lock);
    orders.sort_unstable_by(|a, b| {
        let ap = a.price.parse::<f64>().unwrap();
        let bp = b.price.parse::<f64>().unwrap();
        if ap > bp {
            Ordering::Greater
        } else if ap < bp {
            Ordering::Less
        } else {
            Ordering::Equal
        }
    });
    let sell_orders: Vec<ListItem> = orders
        .iter()
        .filter_map(|o| {
            let s = match o.side {
                Side::Buy => {
                    return None;
                }
                Side::Sell => Style::default().fg(Color::Red),
            };
            let content = vec![Spans::from(vec![
                Span::styled(format!("{:<10}", o.price), s),
                Span::raw(o.quantity.clone()),
            ])];
            Some(ListItem::new(content))
        })
        .collect();
    let buy_orders: Vec<ListItem> = orders
        .iter()
        .rev()
        .filter_map(|o| {
            let s = match o.side {
                Side::Buy => Style::default().fg(Color::Green),
                Side::Sell => {
                    return None;
                }
            };
            let content = vec![Spans::from(vec![
                Span::styled(format!("{:<10}", o.price), s),
                Span::raw(o.quantity.clone()),
            ])];
            Some(ListItem::new(content))
        })
        .collect();
    let sell_orders_list = List::new(sell_orders)
        .block(Block::default().borders(Borders::ALL).title("Sell Orders"))
        .start_corner(Corner::BottomLeft);
    let buy_orders_list = List::new(buy_orders)
        .block(Block::default().borders(Borders::ALL).title("Buy Orders"))
        .start_corner(Corner::TopLeft);
    let limit = { *min_sell_price.lock().unwrap() };
    let mut status_spans = vec![Span::raw(format!("{}", limit))];
    if ignore_coinbase_price.load(atomic::Ordering::Relaxed) {
        status_spans.push(Span::raw("  "));
        status_spans.push(Span::styled("IGNORING CB PRICE", Style::default().fg(Color::Red).add_modifier(tui::style::Modifier::BOLD)));
    };
    let status = Paragraph::new(vec![Spans::from(status_spans)]).block(
        Block::default()
            .title("Min Sell Price")
            .borders(Borders::ALL),
    );
    f.render_widget(status, chunks[0]);
    f.render_widget(sell_orders_list, chunks[1]);
    f.render_widget(buy_orders_list, chunks[2]);
}

fn draw_price_history<B: Backend>(
    f: &mut Frame<B>,
    state: &mut TableState,
    price_history: Arc<Mutex<PriceHistory>>,
    area: Rect,
) {
    let lock = price_history.lock().unwrap();
    let (_avg1h, min1h, max1h) = lock.stats(Duration::from_secs(60 * 60));
    let (_avg24h, min24h, max24h) = lock.stats(Duration::from_secs(60 * 60 * 24));
    let cur = lock.last().unwrap_or(0.0);
    drop(lock);

    let header_cells = ["", "Min", "Current", "Max"].iter().map(|h| {
        Cell::from(*h).style(
            Style::default()
                .fg(Color::Blue)
                .add_modifier(Modifier::BOLD),
        )
    });
    let header = Row::new(header_cells).height(1);
    let table = Table::new(vec![
        Row::new(vec![
            Cell::from("1 hour"),
            Cell::from(format!("{:.2}", min1h)),
            Cell::from(format!("{:.2}", cur)),
            Cell::from(format!("{:.2}", max1h)),
        ])
        .style(Style::default().fg(Color::Blue)),
        Row::new(vec![
            Cell::from("24 hours"),
            Cell::from(format!("{:.2}", min24h)),
            Cell::from(""),
            Cell::from(format!("{:.2}", max24h)),
        ])
        .style(Style::default().fg(Color::Blue)),
    ])
    .header(header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title("Coinbase Sell Price"),
    )
    .widths(&[
        Constraint::Percentage(25),
        Constraint::Percentage(25),
        Constraint::Percentage(25),
        Constraint::Percentage(25),
    ]);

    f.render_stateful_widget(table, area, state);
}

fn draw_trade_history<B: Backend>(
    f: &mut Frame<B>,
    state: &mut TableState,
    trade_history: Arc<Mutex<Vec<FillsData>>>,
    area: Rect,
) {
    let lock = trade_history.lock().unwrap();
    let orders = lock
        .iter()
        .map(|(date, typ, quantity, price)| {
            let typ = match typ {
                Type::Market => "M".to_string(),
                Type::Limit => format!("L({})", price),
            };
            Row::new(vec![
                Cell::from(format!("{}", date.format("%Y-%m-%d %H:%M:%S"))),
                Cell::from(typ),
                Cell::from(format!("{:5}", quantity)),
                Cell::from(price.clone()),
            ])
        })
        .collect::<Vec<_>>();
    drop(lock);

    let header_cells = ["Created", "Type", "Quantity", "Price"]
        .iter()
        .map(|h| Cell::from(*h).style(Style::default().fg(Color::White)));
    let header = Row::new(header_cells).height(1);
    let table = Table::new(orders)
        .header(header)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Trade History"),
        )
        .widths(&[
            Constraint::Percentage(35),
            Constraint::Percentage(25),
            Constraint::Percentage(20),
            Constraint::Percentage(20),
        ]);

    f.render_stateful_widget(table, area, state);
}

fn draw_assets<B: Backend>(
    f: &mut Frame<B>,
    state: &mut TableState,
    assets: Arc<Mutex<HashMap<&'static str, f64>>>,
    area: Rect,
) {
    let lock = assets.lock().unwrap();
    let assets = lock
        .iter()
        .map(|(k, v)| Row::new(vec![Cell::from(*k), Cell::from(format!("{:5}", v))]))
        .collect::<Vec<_>>();
    drop(lock);
    let table = Table::new(assets)
        .block(Block::default().borders(Borders::ALL).title("Assets"))
        .widths(&[Constraint::Percentage(20), Constraint::Percentage(80)]);
    f.render_stateful_widget(table, area, state);
}

async fn fetch_account_fills(
    nbxclient: &Client,
    account_id: &str,
    market: &str,
    limit: usize,
) -> anyhow::Result<Vec<FillsData>> {
    let mut fills = Vec::new();
    let mut resp = nbxclient.accounts_orders(account_id).await?;
    fills.extend(resp.orders.iter().flat_map(|o| {
        if o.market == market {
            order_flat_map(o)
        } else {
            vec![]
        }
    }));
    while let Some(next) = resp.next_page().await? {
        fills.extend(next.orders.iter().flat_map(|o| {
            if o.market == market {
                order_flat_map(o)
            } else {
                vec![]
            }
        }));
        resp = next;
        if fills.len() >= limit {
            break;
        }
    }
    Ok(fills)
}

fn order_flat_map(o: &accounts::Order) -> Vec<FillsData> {
    let typ = o.execution.typ;
    o.fills
        .iter()
        .filter_map(|fill| {
            if let (Ok(q), Ok(c)) = (
                fill.quantity.parse::<f64>(),
                chrono::Utc.datetime_from_str(&fill.created_at, "%Y-%m-%dT%H:%M:%S%.6f%:z"),
            ) {
                let c = c.with_timezone(&chrono::Local);
                Some((c, typ, q, fill.price.clone()))
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
}
