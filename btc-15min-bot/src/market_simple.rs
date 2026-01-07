//! Real market discovery for Bitcoin 15min markets
//!
//! This version uses the Gamma API to discover active markets

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use polymarket_client_sdk::gamma::Client as GammaClient;
use polymarket_client_sdk::gamma::types::request::EventsRequest;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

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

/// Real market discovery using Gamma API
pub struct MarketDiscovery {
    client: GammaClient,
    query: String,
}

impl MarketDiscovery {
    /// Create a new market discovery instance
    pub fn new(client: GammaClient, query: String) -> Self {
        Self { client, query }
    }

    /// Find the next upcoming market using live API data
    pub async fn find_next_upcoming_market(&self) -> Result<Option<BtcMarket>> {
        info!("🔍 Searching for active Bitcoin Up or Down EVENTS via Gamma API...");

        // Use Gamma API to fetch events (not markets!)
        let mut offset = 0;
        let limit = 100;
        let max_iterations = 50; // Fetch up to 5000 events
        let mut all_events = Vec::new();

        for iteration in 0..max_iterations {
            info!("   Fetching batch {} (offset: {})...", iteration + 1, offset);

            let request = EventsRequest::builder()
                .limit(limit)
                .offset(offset)
                .active(true) // Only active events
                .build();

            let events = self
                .client
                .events(&request)
                .await
                .context("Failed to fetch events from Gamma API")?;

            info!("   Fetched {} events", events.len());

            if events.is_empty() {
                break;
            }

            // Log sample slugs on first batch
            if iteration == 0 {
                for (i, event) in events.iter().take(3).enumerate() {
                    if let Some(slug) = &event.slug {
                        info!("   Sample event slug {}: {}", i + 1, slug);
                    }
                }
            }

            // Count BTC 15min events in this batch
            let btc_count = events.iter()
                .filter(|e| e.slug.as_ref().map_or(false, |s| s.contains("btc-updown-15m")))
                .count();

            if btc_count > 0 {
                info!("   ✅ Found {} BTC 15min events in this batch", btc_count);
            }

            all_events.extend(events);

            // Stop early if we found BTC events
            if btc_count > 0 {
                break;
            }

            offset += limit;
        }

        info!("   Total events fetched: {}", all_events.len());

        // Filter for Bitcoin Up or Down events and extract markets
        let mut btc_markets = Vec::new();
        let now = Utc::now();

        for event in &all_events {
            // Match by event slug pattern "btc-updown-15m-{timestamp}"
            let slug = match &event.slug {
                Some(s) => s,
                None => continue,
            };

            if !slug.contains("btc-updown-15m") {
                continue;
            }

            // Skip closed events
            if event.closed.unwrap_or(false) {
                continue;
            }

            // Events contain markets - get the first market from the event
            let markets = match &event.markets {
                Some(m) if !m.is_empty() => m,
                _ => continue,
            };

            // Bitcoin Up or Down events should have exactly 1 market with 2 outcomes
            if markets.len() != 1 {
                continue;
            }

            let market = &markets[0];

            // Check if market has outcomes (clob_token_ids)
            let token_ids = match &market.clob_token_ids {
                Some(ids) if !ids.is_empty() => ids,
                _ => continue,
            };

            // Parse comma-separated token IDs
            let tokens: Vec<&str> = token_ids.split(',').collect();
            if tokens.len() != 2 {
                continue;
            }

            let up_token_id = tokens[0].trim().to_string();
            let down_token_id = tokens[1].trim().to_string();

            // Parse event timing
            let start_time = event.start_date.unwrap_or(now);
            let end_time = event.end_date.unwrap_or(start_time + chrono::Duration::minutes(15));

            btc_markets.push(BtcMarket {
                condition_id: market.condition_id.clone().unwrap_or_default(),
                question: market.question.clone()
                    .or_else(|| event.title.clone())
                    .unwrap_or_else(|| "Unknown".to_string()),
                start_time,
                end_time,
                up_token_id,
                down_token_id,
                active: event.active.unwrap_or(true),
                closed: event.closed.unwrap_or(false),
            });
        }

        info!("   Total BTC 15min markets found: {}", btc_markets.len());

        if btc_markets.is_empty() {
            warn!("⚠️  No active Bitcoin Up or Down markets found");
            warn!("⚠️  The bot will retry in the next discovery cycle");
            return Ok(None);
        }

        // Log first few markets
        for (i, market) in btc_markets.iter().take(3).enumerate() {
            info!("   Market #{}: {} (starts: {})", i + 1, market.question, market.start_time);
        }

        // Filter to only upcoming markets (start time in future)
        btc_markets.retain(|m| m.start_time > now);

        if btc_markets.is_empty() {
            warn!("⚠️  No upcoming Bitcoin Up or Down markets found");
            return Ok(None);
        }

        // Sort by start time and get next upcoming market
        btc_markets.sort_by(|a, b| a.start_time.cmp(&b.start_time));
        let market = btc_markets.into_iter().next().unwrap();

        info!("✅ Selected next upcoming market: {}", market.question);
        info!("   Condition ID: {}", &market.condition_id);
        info!("   UP Token:     {}...", &market.up_token_id[..20]);
        info!("   DOWN Token:   {}...", &market.down_token_id[..20]);
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
