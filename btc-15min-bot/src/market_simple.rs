//! Real market discovery for Bitcoin 15min markets
//!
//! This version uses the CLOB API to discover active markets

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use polymarket_client_sdk::clob::Client;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

/// Represents a Bitcoin 15min market
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BtcMarket {
    /// Unique condition ID for the market
    pub condition_id: String,

    /// Market question/description
    pub question: String,

    /// Market start time (UTC)
    pub start_time: DateTime<Utc>,

    /// Market end time (UTC)
    pub end_time: DateTime<Utc>,

    /// Token ID for UP outcome
    pub up_token_id: String,

    /// Token ID for DOWN outcome
    pub down_token_id: String,

    /// Whether market is active
    pub active: bool,

    /// Whether market has closed
    pub closed: bool,
}

/// Market outcome side
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MarketSide {
    Up,
    Down,
}

impl MarketSide {
    pub fn opposite(&self) -> Self {
        match self {
            Self::Up => Self::Down,
            Self::Down => Self::Up,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Up => "UP",
            Self::Down => "DOWN",
        }
    }
}

impl std::fmt::Display for MarketSide {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Real market discovery using CLOB API
pub struct MarketDiscovery {
    client: Client,
    query: String,
}

impl MarketDiscovery {
    /// Create a new market discovery instance
    pub fn new(client: Client, query: String) -> Self {
        Self { client, query }
    }

    /// Find the next upcoming market using live API data
    pub async fn find_next_upcoming_market(&self) -> Result<Option<BtcMarket>> {
        info!("🔍 Searching for active markets matching: '{}'", self.query);

        // First, try to get full market details (not simplified)
        let page = self
            .client
            .markets(None)
            .await
            .context("Failed to fetch markets from CLOB API")?;

        info!("   Fetched {} markets total", page.data.len());

        // Log first few active markets for debugging
        let mut logged = 0;
        for market in &page.data {
            if market.active && !market.closed && logged < 3 {
                info!("   Sample market: {}", market.question);
                logged += 1;
            }
        }

        let mut btc_markets = Vec::new();
        let now = Utc::now();

        for market in &page.data {
            // Search in question field AND market slug (case-insensitive)
            let question_lower = market.question.to_lowercase();
            let slug_lower = market.market_slug.to_lowercase();
            let query_lower = self.query.to_lowercase();

            // Match if query appears in question OR slug contains "btc-updown-15m"
            let matches_query = question_lower.contains(&query_lower);
            let is_btc_15min = slug_lower.contains("btc-updown-15m");

            if !matches_query && !is_btc_15min {
                continue;
            }

            // Skip closed or inactive markets
            if market.closed || !market.active || !market.accepting_orders {
                continue;
            }

            // Check if we have exactly 2 tokens (UP and DOWN)
            if market.tokens.len() != 2 {
                continue;
            }

            // Extract token IDs
            let up_token_id = market.tokens[0].token_id.clone();
            let down_token_id = market.tokens[1].token_id.clone();

            // Try to parse market timing from game_start_time or end_date_iso
            let start_time = market.game_start_time.unwrap_or(now);
            let end_time = market.end_date_iso.unwrap_or(now + chrono::Duration::minutes(15));

            btc_markets.push(BtcMarket {
                condition_id: market.condition_id.clone(),
                question: market.question.clone(),
                start_time,
                end_time,
                up_token_id,
                down_token_id,
                active: market.active,
                closed: market.closed,
            });

            info!("   ✅ Matched: {} (slug: {})", market.question, market.market_slug);
        }

        if btc_markets.is_empty() {
            warn!("⚠️  No active markets found matching '{}'", self.query);
            warn!("⚠️  Try changing 'market_query' in config.toml");
            warn!("⚠️  The bot will retry in the next discovery cycle");
            return Ok(None);
        }

        // Sort by start time and get next upcoming market
        btc_markets.sort_by(|a, b| a.start_time.cmp(&b.start_time));
        let market = btc_markets.into_iter().next().unwrap();

        info!("✅ Selected market: {}", market.question);
        info!("   Condition ID: {}", &market.condition_id);
        info!("   UP Token:     {}", &market.up_token_id[..20]);
        info!("   DOWN Token:   {}", &market.down_token_id[..20]);
        info!("   Start time:   {}", market.start_time);
        info!("   End time:     {}", market.end_time);

        Ok(Some(market))
    }

    /// Check if a market has started
    pub fn has_market_started(market: &BtcMarket) -> bool {
        Utc::now() >= market.start_time
    }

    /// Check if a market has ended
    pub fn has_market_ended(market: &BtcMarket) -> bool {
        Utc::now() >= market.end_time
    }

    /// Get time until market starts (in seconds)
    pub fn seconds_until_start(market: &BtcMarket) -> i64 {
        (market.start_time - Utc::now()).num_seconds()
    }

    /// Get time until market ends (in seconds)
    pub fn seconds_until_end(market: &BtcMarket) -> i64 {
        (market.end_time - Utc::now()).num_seconds()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_market_side_display() {
        assert_eq!(MarketSide::Up.to_string(), "UP");
        assert_eq!(MarketSide::Down.to_string(), "DOWN");
    }

    #[test]
    fn test_market_side_opposite() {
        assert_eq!(MarketSide::Up.opposite(), MarketSide::Down);
        assert_eq!(MarketSide::Down.opposite(), MarketSide::Up);
    }

    #[tokio::test]
    async fn test_find_market() {
        let discovery = MarketDiscovery::new("Bitcoin 15min".to_string());

        // This will return None because token IDs aren't configured
        let result = discovery.find_next_upcoming_market().await;
        assert!(result.is_ok());
    }
}
