use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use color_eyre::eyre::{Result, eyre};
use serde_json::Value;
use tracing::{debug, info, warn};

use crate::model::message::TokenUsage;

/// Pricing for a single model (USD per million tokens).
#[derive(Clone, Debug)]
pub struct ModelPricing {
    pub input: f64,
    pub output: f64,
    pub cache_read: Option<f64>,
    pub reasoning: Option<f64>,
    /// Context window size from models.dev limit.context.
    pub context: Option<u32>,
}

/// Catalog of model pricing fetched from models.dev.
#[derive(Clone, Debug)]
pub struct PricingCatalog {
    /// provider_id -> model_id -> pricing
    providers: HashMap<String, HashMap<String, ModelPricing>>,
}

/// Cache TTL for the pricing catalog on disk.
const CACHE_TTL: Duration = Duration::from_secs(5 * 60);

impl PricingCatalog {
    /// Fetch the pricing catalog from models.dev and cache to disk.
    pub async fn fetch_and_cache() -> Result<Self> {
        let cache_path = Self::cache_path()?;

        // Check disk cache first
        if let Ok(metadata) = std::fs::metadata(&cache_path) {
            if let Ok(modified) = metadata.modified() {
                if SystemTime::now()
                    .duration_since(modified)
                    .unwrap_or(Duration::MAX)
                    < CACHE_TTL
                {
                    if let Ok(catalog) = Self::from_cached(&cache_path) {
                        debug!("Using cached pricing catalog");
                        return Ok(catalog);
                    }
                }
            }
        }

        // Fetch fresh
        info!("Fetching pricing catalog from models.dev");
        let body = reqwest::get("https://models.dev/api.json")
            .await
            .map_err(|e| eyre!("Failed to fetch models.dev: {}", e))?
            .text()
            .await
            .map_err(|e| eyre!("Failed to read models.dev response: {}", e))?;

        let catalog = Self::parse(&body)?;

        // Cache to disk (best-effort)
        if let Some(parent) = cache_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(&cache_path, &body) {
            warn!("Failed to cache pricing catalog: {}", e);
        } else {
            debug!("Cached pricing catalog to {:?}", cache_path);
        }

        Ok(catalog)
    }

    /// Load catalog from a cached JSON file.
    fn from_cached(path: &Path) -> Result<Self> {
        let body = std::fs::read_to_string(path)
            .map_err(|e| eyre!("Failed to read cached catalog: {}", e))?;
        Self::parse(&body)
    }

    /// Parse the api.json response into a PricingCatalog.
    fn parse(json: &str) -> Result<Self> {
        let root: HashMap<String, Value> = serde_json::from_str(json)
            .map_err(|e| eyre!("Failed to parse models.dev JSON: {}", e))?;

        let mut providers = HashMap::new();

        for (provider_id, provider_val) in &root {
            let Some(models_obj) = provider_val.get("models").and_then(|v| v.as_object()) else {
                continue;
            };

            let mut models = HashMap::new();

            for (model_id, model_val) in models_obj {
                let Some(cost) = model_val.get("cost").and_then(|v| v.as_object()) else {
                    continue;
                };

                let input = cost.get("input").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let output = cost.get("output").and_then(|v| v.as_f64()).unwrap_or(0.0);

                // Skip models with no pricing
                if input == 0.0 && output == 0.0 {
                    continue;
                }

                let cache_read = cost.get("cache_read").and_then(|v| v.as_f64());
                let reasoning = cost.get("reasoning").and_then(|v| v.as_f64());

                let context = model_val
                    .get("limit")
                    .and_then(|l| l.get("context"))
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32);

                models.insert(
                    model_id.clone(),
                    ModelPricing {
                        input,
                        output,
                        cache_read,
                        reasoning,
                        context,
                    },
                );
            }

            if !models.is_empty() {
                providers.insert(provider_id.clone(), models);
            }
        }

        info!("Parsed pricing catalog: {} providers", providers.len());
        Ok(Self { providers })
    }

    /// Number of providers in the catalog.
    pub fn provider_count(&self) -> usize {
        self.providers.len()
    }

    /// Look up pricing for a provider/model pair.
    pub fn lookup(&self, provider: &str, model: &str) -> Option<&ModelPricing> {
        self.providers
            .get(provider)
            .and_then(|models| models.get(model))
    }

    /// Calculate the cost in USD for a given usage.
    pub fn calculate_cost(&self, provider: &str, model: &str, usage: &TokenUsage) -> Option<f64> {
        let pricing = self.lookup(provider, model)?;

        let mut cost = 0.0;

        // Input tokens (prompt minus cached)
        if let Some(prompt) = usage.prompt_tokens {
            let cached = usage.cached_tokens.unwrap_or(0);
            let non_cached = (prompt - cached).max(0);
            cost += (non_cached as f64 / 1_000_000.0) * pricing.input;

            // Cached tokens at the cache_read rate
            if cached > 0 {
                if let Some(cache_rate) = pricing.cache_read {
                    cost += (cached as f64 / 1_000_000.0) * cache_rate;
                }
            }
        }

        // Output tokens (completion minus reasoning)
        if let Some(completion) = usage.completion_tokens {
            let reasoning = usage.reasoning_tokens.unwrap_or(0);
            let non_reasoning = (completion - reasoning).max(0);
            cost += (non_reasoning as f64 / 1_000_000.0) * pricing.output;

            // Reasoning tokens at the reasoning rate (or output rate if no reasoning rate)
            if reasoning > 0 {
                let rate = pricing.reasoning.unwrap_or(pricing.output);
                cost += (reasoning as f64 / 1_000_000.0) * rate;
            }
        }

        Some(cost)
    }

    /// Get the context window size for a provider/model pair.
    pub fn context_window(&self, provider: &str, model: &str) -> Option<u32> {
        self.lookup(provider, model).and_then(|p| p.context)
    }

    /// Path to the cached pricing catalog file.
    fn cache_path() -> Result<PathBuf> {
        let data_dir =
            dirs::data_dir().ok_or_else(|| eyre!("Could not determine data directory"))?;
        Ok(data_dir.join("ozma").join("cache").join("models.json"))
    }

    /// Map an ozma provider name to a models.dev provider ID.
    ///
    /// Uses the adapter field if available, otherwise lowercases the name.
    /// Handles known aliases (e.g., "gemini" → "google").
    pub fn map_provider_id(name: &str, adapter: Option<&str>) -> String {
        let id = adapter.unwrap_or(name).to_lowercase();
        match id.as_str() {
            "gemini" => "google".to_string(),
            other => other.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{
      "openai": {
        "models": {
          "gpt-4o": {
            "cost": {
              "input": 2.5,
              "output": 10.0,
              "cache_read": 1.25,
              "reasoning": 15.0
            },
            "limit": { "context": 128000 }
          },
          "free-model": {
            "cost": { "input": 0.0, "output": 0.0 }
          }
        }
      },
      "empty-provider": {
        "models": {}
      }
    }"#;

    fn catalog() -> PricingCatalog {
        PricingCatalog::parse(FIXTURE).unwrap()
    }

    #[test]
    fn parse_skips_zero_cost_and_empty_providers() {
        let catalog = catalog();
        assert_eq!(catalog.provider_count(), 1);
        assert!(catalog.lookup("openai", "gpt-4o").is_some());
        assert!(catalog.lookup("openai", "free-model").is_none());
        assert!(catalog.lookup("empty-provider", "x").is_none());
    }

    #[test]
    fn lookup_and_context_window() {
        let catalog = catalog();
        let pricing = catalog.lookup("openai", "gpt-4o").unwrap();
        assert_eq!(pricing.input, 2.5);
        assert_eq!(pricing.output, 10.0);
        assert_eq!(pricing.cache_read, Some(1.25));
        assert_eq!(pricing.reasoning, Some(15.0));
        assert_eq!(catalog.context_window("openai", "gpt-4o"), Some(128_000));
        assert_eq!(catalog.context_window("openai", "missing"), None);
    }

    #[test]
    fn calculate_cost_with_cache_and_reasoning() {
        let catalog = catalog();
        let usage = TokenUsage {
            prompt_tokens: Some(1_000_000),
            completion_tokens: Some(1_000_000),
            total_tokens: Some(2_000_000),
            cached_tokens: Some(400_000),
            reasoning_tokens: Some(200_000),
        };
        // non-cached input: 600k * 2.5 / 1M = 1.5
        // cached: 400k * 1.25 / 1M = 0.5
        // non-reasoning output: 800k * 10 / 1M = 8.0
        // reasoning: 200k * 15 / 1M = 3.0
        let cost = catalog.calculate_cost("openai", "gpt-4o", &usage).unwrap();
        assert!((cost - 13.0).abs() < 1e-9);
    }

    #[test]
    fn calculate_cost_missing_model_returns_none() {
        let catalog = catalog();
        let usage = TokenUsage {
            prompt_tokens: Some(1000),
            completion_tokens: Some(1000),
            ..Default::default()
        };
        assert!(catalog.calculate_cost("openai", "nope", &usage).is_none());
    }

    #[test]
    fn map_provider_id_aliases_and_adapter() {
        assert_eq!(PricingCatalog::map_provider_id("gemini", None), "google");
        assert_eq!(
            PricingCatalog::map_provider_id("MyProvider", Some("OpenAI")),
            "openai"
        );
        assert_eq!(PricingCatalog::map_provider_id("Anthropic", None), "anthropic");
    }

    #[test]
    fn parse_invalid_json_errors() {
        assert!(PricingCatalog::parse("not json").is_err());
    }
}
