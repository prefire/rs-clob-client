//! Real order execution with Polymarket API
//!
//! This version places ACTUAL trades on Polymarket

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use alloy::signers::local::PrivateKeySigner;
use polymarket_client_sdk::auth::state::Authenticated;
use polymarket_client_sdk::auth::Normal;
use polymarket_client_sdk::clob::Client;
use polymarket_client_sdk::clob::types::{Amount, Side};
use polymarket_client_sdk::clob::types::request::{LastTradePriceRequest, MidpointRequest, PriceRequest};
use polymarket_client_sdk::types::Decimal;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::config::Config;
use crate::market_simple::{BtcMarket, MarketSide};
use crate::strategy_simple::TradingSignal;
use crate::trailing_stop::{ExitReason, TrailingStop};

/// Active position
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub market: BtcMarket,
    pub side: MarketSide,
    pub token_id: String,
    pub entry_price: Decimal,
    pub size_usdc: Decimal,
    pub shares: Decimal,
    pub entry_time: DateTime<Utc>,
    pub order_ids: Vec<String>,
    pub trailing_stop: TrailingStop,
}

/// Real trader with authenticated client
pub struct RealTrader {
    client: Client<Authenticated<Normal>>,
    signer: PrivateKeySigner,
    config: Config,
    current_position: Option<Position>,
}

impl RealTrader {
    /// Create a new real trader
    pub fn new(
        client: Client<Authenticated<Normal>>,
        signer: PrivateKeySigner,
        config: Config,
    ) -> Self {
        Self {
            client,
            signer,
            config,
            current_position: None,
        }
    }

    /// Check if there's an open position
    pub fn has_position(&self) -> bool {
        self.current_position.is_some()
    }

    /// Get current position
    pub fn position(&self) -> Option<&Position> {
        self.current_position.as_ref()
    }

    /// Enter a position (REAL TRADE)
    pub async fn enter_position(
        &mut self,
        market: &BtcMarket,
        signal: &TradingSignal,
    ) -> Result<Position> {
        if self.has_position() {
            anyhow::bail!("Already have an open position");
        }

        info!("🚀 Entering {} position on: {}", signal.side, market.question);

        // Determine which token to buy
        let token_id = match signal.side {
            MarketSide::Up => &market.up_token_id,
            MarketSide::Down => &market.down_token_id,
        };

        // Get current price
        let current_price = self.get_token_price(token_id).await?;

        info!("   Current {} price: {:.4}", signal.side, current_price);

        // Calculate shares to buy
        let shares = self.config.strategy.trade_size_usdc / current_price;

        info!("   Placing order for {:.2} shares at ${:.2}", shares, self.config.strategy.trade_size_usdc);

        // Place the order
        if self.config.operational.dry_run {
            warn!("   💰 DRY RUN: Would place market order");
        } else {
            self.place_market_order(token_id, shares).await?;
        }

        // Create position
        let position = Position {
            market: market.clone(),
            side: signal.side,
            token_id: token_id.clone(),
            entry_price: current_price,
            size_usdc: self.config.strategy.trade_size_usdc,
            shares,
            entry_time: Utc::now(),
            order_ids: vec![],
            trailing_stop: TrailingStop::new(
                current_price,
                signal.side,
                self.config.risk.trailing_stop_percentage,
                self.config.risk.min_profit_target,
            ),
        };

        info!(
            "   ✅ Position opened: {} {:.2} shares @ {:.4} (${:.2})",
            position.side,
            position.shares,
            position.entry_price,
            position.size_usdc
        );

        self.current_position = Some(position.clone());
        Ok(position)
    }

    /// Exit current position (REAL TRADE)
    pub async fn exit_position(&mut self, reason: ExitReason) -> Result<()> {
        let position = self
            .current_position
            .as_ref()
            .context("No position to exit")?
            .clone();

        let current_price = position.trailing_stop.current_price();
        let profit_pct = position.trailing_stop.current_profit_percentage();
        let profit_usdc = position.trailing_stop.current_profit_usdc(position.size_usdc);

        info!("🔚 Exiting {} position | Reason: {}", position.side, reason);
        info!(
            "   Exit price: {:.4} | P&L: ${:.2} ({:.2}%)",
            current_price,
            profit_usdc,
            profit_pct * Decimal::ONE_HUNDRED
        );

        if self.config.operational.dry_run {
            warn!("   💰 DRY RUN: Would sell {:.2} shares", position.shares);
        } else {
            self.place_market_sell(&position.token_id, position.shares).await?;
        }

        self.current_position = None;
        info!("   ✅ Position closed");
        Ok(())
    }

    /// Update position and check trailing stop
    pub async fn update_position(&mut self) -> Result<Option<ExitReason>> {
        if self.current_position.is_none() {
            return Ok(None);
        }

        // Get current price for the position's token
        let token_id = self.current_position.as_ref().unwrap().token_id.clone();
        let current_price = self.get_token_price(&token_id).await?;

        // Update the position's trailing stop
        let position = self.current_position.as_mut().unwrap();

        debug!(
            "   📊 Price update: {:.4} | P&L: {:.2}%",
            current_price,
            position.trailing_stop.current_profit_percentage() * Decimal::ONE_HUNDRED
        );

        let exit_reason = position.trailing_stop.update(current_price);

        // Check maximum hold time
        if exit_reason.is_none() {
            let hold_time = position.trailing_stop.time_since_entry().num_seconds() as u64;
            if hold_time >= self.config.risk.max_position_hold_seconds {
                warn!("   ⏰ Maximum hold time exceeded");
                return Ok(Some(ExitReason::MaxHoldTime));
            }
        }

        Ok(exit_reason)
    }

    /// Get current price for a token (REAL API CALL)
    async fn get_token_price(&self, token_id: &str) -> Result<Decimal> {
        // Try last trade price first
        let request = LastTradePriceRequest::builder()
            .token_id(token_id.to_string())
            .build();

        if let Ok(response) = self.client.last_trade_price(&request).await {
            return Ok(response.price);
        }

        // Fallback to midpoint
        let request = MidpointRequest::builder()
            .token_id(token_id.to_string())
            .build();

        if let Ok(response) = self.client.midpoint(&request).await {
            return Ok(response.mid);
        }

        // Fallback to price endpoint
        let request = PriceRequest::builder()
            .token_id(token_id.to_string())
            .side(Side::Buy)
            .build();

        let response = self.client.price(&request).await?;
        Ok(response.price)
    }

    /// Place a market buy order (REAL API CALL)
    async fn place_market_order(&self, token_id: &str, shares: Decimal) -> Result<()> {
        info!("   📝 Placing market BUY order for {:.2} shares", shares);

        // Build market order
        let order = self
            .client
            .market_order()
            .token_id(token_id)
            .amount(Amount::usdc(shares)?)
            .side(Side::Buy)
            .build()
            .await?;

        // Sign order
        let signed_order = self.client.sign(&self.signer, order).await?;

        // Post order
        let response = self.client.post_order(signed_order).await?;

        info!("   ✅ Order placed successfully | Order ID: {}", response.order_id);

        Ok(())
    }

    /// Place a market sell order (REAL API CALL)
    async fn place_market_sell(&self, token_id: &str, shares: Decimal) -> Result<()> {
        info!("   📝 Placing market SELL order for {:.2} shares", shares);

        // Build market order
        let order = self
            .client
            .market_order()
            .token_id(token_id)
            .amount(Amount::usdc(shares)?)
            .side(Side::Sell)
            .build()
            .await?;

        // Sign order
        let signed_order = self.client.sign(&self.signer, order).await?;

        // Post order
        let response = self.client.post_order(signed_order).await?;

        info!("   ✅ Order placed successfully | Order ID: {}", response.order_id);

        Ok(())
    }
}
