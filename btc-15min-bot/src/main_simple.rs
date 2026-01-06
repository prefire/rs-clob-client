//! Bitcoin 15min Trading Bot - Simplified Version
//!
//! This version runs in DRY RUN mode only and simulates trading.
//! Perfect for learning and testing the bot logic!

mod config;
mod market_simple;
mod strategy_simple;
mod trader_simple;
mod trailing_stop;
mod utils;

use anyhow::Result;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use crate::config::Config;
use crate::market_simple::{BtcMarket, MarketDiscovery};
use crate::strategy_simple::VolumeStrategy;
use crate::trader_simple::Trader;
use crate::utils::{format_duration, sleep_seconds, sleep_until};

/// Bot state machine
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BotState {
    Discovery,
    Waiting,
    Analyzing,
    InPosition,
    Error,
}

/// Main bot
struct Bot {
    trader: Trader,
    market_discovery: MarketDiscovery,
    strategy: VolumeStrategy,
    config: Config,
    state: BotState,
    current_market: Option<BtcMarket>,
}

impl Bot {
    /// Create new bot
    fn new(config: Config) -> Result<Self> {
        info!("🤖 Initializing Bitcoin 15min Trading Bot...");

        let market_discovery = MarketDiscovery::new(config.strategy.market_query.clone());
        let strategy = VolumeStrategy::new(config.strategy.clone());
        let trader = Trader::new(config.clone());

        Ok(Self {
            trader,
            market_discovery,
            strategy,
            config,
            state: BotState::Discovery,
            current_market: None,
        })
    }

    /// Main bot loop
    async fn run(&mut self) -> Result<()> {
        info!("🚀 Starting bot main loop...");

        if self.config.operational.dry_run {
            warn!("⚠️  DRY RUN MODE - No real trades will be executed ⚠️");
            warn!("⚠️  Prices and volumes are SIMULATED for testing ⚠️");
        }

        loop {
            match self.state {
                BotState::Discovery => {
                    if let Err(e) = self.discover_next_market().await {
                        error!("❌ Market discovery failed: {}", e);
                        self.state = BotState::Error;
                        continue;
                    }
                }

                BotState::Waiting => {
                    if let Err(e) = self.wait_for_entry_timing().await {
                        error!("❌ Wait phase failed: {}", e);
                        self.state = BotState::Error;
                        continue;
                    }
                }

                BotState::Analyzing => {
                    if let Err(e) = self.analyze_and_enter().await {
                        error!("❌ Analysis/entry failed: {}", e);
                        self.state = BotState::Discovery;
                        continue;
                    }
                }

                BotState::InPosition => {
                    if let Err(e) = self.monitor_position().await {
                        error!("❌ Position monitoring failed: {}", e);
                        self.state = BotState::Error;
                        continue;
                    }
                }

                BotState::Error => {
                    warn!("⚠️  In error state, recovering...");
                    self.handle_error().await;
                }
            }

            sleep_seconds(1).await;
        }
    }

    /// Discover next market
    async fn discover_next_market(&mut self) -> Result<()> {
        info!("🔍 Discovering next upcoming market...");

        let market = self.market_discovery.find_next_upcoming_market().await?;

        match market {
            Some(m) => {
                let time_until_start = MarketDiscovery::seconds_until_start(&m);

                info!(
                    "✅ Found market: {} | Starts in: {}",
                    m.question,
                    format_duration(chrono::Duration::seconds(time_until_start))
                );

                self.current_market = Some(m);
                self.state = BotState::Waiting;
            }
            None => {
                warn!("⚠️  No upcoming markets found, retrying in 5 minutes...");
                sleep_seconds(self.config.operational.market_refresh_interval_seconds).await;
            }
        }

        Ok(())
    }

    /// Wait for entry timing
    async fn wait_for_entry_timing(&mut self) -> Result<()> {
        let market = self.current_market.as_ref().ok_or_else(|| anyhow::anyhow!("No current market"))?;

        let time_until_start = MarketDiscovery::seconds_until_start(market);

        if time_until_start <= 0 {
            warn!("⚠️  Market has already started, moving to next");
            self.state = BotState::Discovery;
            return Ok(());
        }

        if time_until_start <= self.config.strategy.entry_timing_seconds as i64 {
            info!("⏰ Entry timing reached, analyzing market...");
            self.state = BotState::Analyzing;
            return Ok(());
        }

        let seconds_to_wait = time_until_start - self.config.strategy.entry_timing_seconds as i64;

        if seconds_to_wait > 30 {
            info!(
                "⏳ Waiting {} until entry timing",
                format_duration(chrono::Duration::seconds(seconds_to_wait))
            );
        }

        let entry_time = market.start_time
            - chrono::Duration::seconds(self.config.strategy.entry_timing_seconds as i64);

        sleep_until(entry_time).await;

        Ok(())
    }

    /// Analyze and enter
    async fn analyze_and_enter(&mut self) -> Result<()> {
        let market = self.current_market.as_ref().ok_or_else(|| anyhow::anyhow!("No current market"))?.clone();

        info!("📊 Analyzing market volume...");

        let signal = self.strategy.analyze_market(&market).await?;

        match signal {
            Some(sig) => {
                info!(
                    "✅ Signal: {} (confidence: {:.2}%)",
                    sig.side,
                    sig.confidence * rust_decimal::Decimal::ONE_HUNDRED
                );

                self.trader.enter_position(&market, &sig).await?;
                self.state = BotState::InPosition;
            }
            None => {
                warn!("⚠️  No trading signal, moving to next market");
                self.state = BotState::Discovery;
            }
        }

        Ok(())
    }

    /// Monitor position
    async fn monitor_position(&mut self) -> Result<()> {
        if !self.trader.has_position() {
            warn!("⚠️  No position to monitor");
            self.state = BotState::Discovery;
            return Ok(());
        }

        let exit_reason = self.trader.update_position().await?;

        if let Some(reason) = exit_reason {
            info!("🎯 Exit condition triggered: {}", reason);
            self.trader.exit_position(reason).await?;
            self.state = BotState::Discovery;
            return Ok(());
        }

        // Check if market has ended
        if let Some(market) = &self.current_market {
            if MarketDiscovery::has_market_ended(market) {
                warn!("⏰ Market has ended, force closing");
                self.trader.exit_position(trailing_stop::ExitReason::MarketEnded).await?;
                self.state = BotState::Discovery;
                return Ok(());
            }
        }

        sleep_seconds(self.config.operational.price_poll_interval_seconds).await;

        Ok(())
    }

    /// Handle error
    async fn handle_error(&mut self) {
        if self.trader.has_position() {
            warn!("⚠️  Attempting to close position due to error");
            if let Err(e) = self.trader.exit_position(trailing_stop::ExitReason::Manual).await {
                error!("❌ Failed to close position: {}", e);
            }
        }

        self.current_market = None;
        self.state = BotState::Discovery;
        warn!("⏳ Waiting 60 seconds before retry...");
        sleep_seconds(60).await;
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Load .env file
    dotenv::dotenv().ok();

    // Initialize logging
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();

    info!("╔═══════════════════════════════════════════════════════════╗");
    info!("║     Bitcoin 15min Trading Bot for Polymarket             ║");
    info!("║     Version 0.1.0 (Simplified / Dry Run Only)            ║");
    info!("╚═══════════════════════════════════════════════════════════╝");

    // Load config
    info!("📂 Loading configuration...");
    let config = Config::load()?;

    info!("✅ Configuration loaded");
    info!("   CLOB Endpoint: {}", config.blockchain.clob_endpoint);
    info!("   Chain ID: {}", config.blockchain.chain_id);
    info!("   Market Query: {}", config.strategy.market_query);
    info!("   Trade Size: ${:.2}", config.strategy.trade_size_usdc);
    info!(
        "   Trailing Stop: {:.2}%",
        config.risk.trailing_stop_percentage * rust_decimal::Decimal::ONE_HUNDRED
    );

    // Create and run bot
    let mut bot = Bot::new(config)?;
    bot.run().await?;

    Ok(())
}
