//! Minimal working trader (dry run only)
//!
//! This simplified version doesn't actually place orders.
//! It just simulates the trading logic for learning purposes.

use anyhow::Result;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

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
    pub trailing_stop: TrailingStop,
}

/// Simplified trader (dry run only)
pub struct Trader {
    config: Config,
    current_position: Option<Position>,
}

impl Trader {
    /// Create a new trader
    pub fn new(config: Config) -> Self {
        Self {
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

    /// Enter a position (SIMULATED)
    pub async fn enter_position(
        &mut self,
        market: &BtcMarket,
        signal: &TradingSignal,
    ) -> Result<Position> {
        if self.has_position() {
            anyhow::bail!("Already have an open position");
        }

        info!("🚀 Entering {} position on: {}", signal.side, market.question);

        // SIMULATED: Use a mock price
        let current_price = Decimal::new(50, 2); // 0.50
        let shares = self.config.strategy.trade_size_usdc / current_price;

        if self.config.operational.dry_run {
            warn!("   💰 DRY RUN: Would buy {:.2} shares at {:.4}", shares, current_price);
        }

        let position = Position {
            market: market.clone(),
            side: signal.side,
            token_id: if signal.side == MarketSide::Up {
                market.up_token_id.clone()
            } else {
                market.down_token_id.clone()
            },
            entry_price: current_price,
            size_usdc: self.config.strategy.trade_size_usdc,
            shares,
            entry_time: Utc::now(),
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

    /// Exit current position (SIMULATED)
    pub async fn exit_position(&mut self, reason: ExitReason) -> Result<()> {
        let position = self
            .current_position
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No position to exit"))?;

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
            warn!("   💰 DRY RUN: Would sell {:.2} shares at {:.4}", position.shares, current_price);
        }

        self.current_position = None;
        info!("   ✅ Position closed");
        Ok(())
    }

    /// Update position with simulated price movement
    pub async fn update_position(&mut self) -> Result<Option<ExitReason>> {
        let position = match self.current_position.as_mut() {
            Some(p) => p,
            None => return Ok(None),
        };

        // SIMULATED: Generate a mock price that moves slightly
        use rand::Rng;
        let mut rng = rand::thread_rng();
        let price_change = rng.gen_range(-0.02..0.02); // ±2%
        let new_price = position.trailing_stop.current_price() * (Decimal::ONE + Decimal::from_f64_retain(price_change).unwrap_or(Decimal::ZERO));

        let new_price = new_price.max(Decimal::new(1, 2)).min(Decimal::new(99, 2)); // Clamp between 0.01 and 0.99

        info!(
            "   📊 Price update: {:.4} | P&L: {:.2}%",
            new_price,
            position.trailing_stop.current_profit_percentage() * Decimal::ONE_HUNDRED
        );

        // Update trailing stop
        let exit_reason = position.trailing_stop.update(new_price);

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
}
