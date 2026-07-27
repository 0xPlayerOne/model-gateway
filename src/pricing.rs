use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PriceScope {
    RuntimeProvider,
    ProviderProfile,
    Canonical,
}

impl PriceScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RuntimeProvider => "runtime_provider",
            Self::ProviderProfile => "provider_profile",
            Self::Canonical => "canonical",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PriceSourceKind {
    Manual,
    ProviderCatalog,
    OfficialApi,
    ModelsDev,
    Aggregate,
    Benchmark,
}

impl PriceSourceKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::ProviderCatalog => "provider_catalog",
            Self::OfficialApi => "official_api",
            Self::ModelsDev => "models_dev",
            Self::Aggregate => "aggregate",
            Self::Benchmark => "benchmark",
        }
    }

    pub const fn fallback_priority(self) -> u8 {
        match self {
            Self::Manual => 0,
            Self::ProviderCatalog => 1,
            Self::OfficialApi => 2,
            Self::ModelsDev => 3,
            Self::Aggregate | Self::Benchmark => 4,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PriceRates {
    pub input_price_per_million: Option<f64>,
    pub output_price_per_million: Option<f64>,
    #[serde(default)]
    pub cache_read_price_per_million: Option<f64>,
    #[serde(default)]
    pub cache_write_price_per_million: Option<f64>,
    #[serde(default)]
    pub reasoning_price_per_million: Option<f64>,
    #[serde(default)]
    pub input_audio_price_per_million: Option<f64>,
    #[serde(default)]
    pub output_audio_price_per_million: Option<f64>,
    #[serde(default)]
    pub request_price: Option<f64>,
    #[serde(default)]
    pub modifiers: BTreeMap<String, Value>,
}

impl PriceRates {
    pub fn validate(&self, model_id: &str) -> Result<(), String> {
        for value in [
            self.input_price_per_million,
            self.output_price_per_million,
            self.cache_read_price_per_million,
            self.cache_write_price_per_million,
            self.reasoning_price_per_million,
            self.input_audio_price_per_million,
            self.output_audio_price_per_million,
            self.request_price,
        ]
        .into_iter()
        .flatten()
        {
            if !value.is_finite() || value < 0.0 {
                return Err(format!(
                    "pricing for '{model_id}' must be finite and non-negative"
                ));
            }
        }
        Ok(())
    }

    pub const fn is_complete(&self) -> bool {
        self.input_price_per_million.is_some() && self.output_price_per_million.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PriceObservation {
    pub source: String,
    pub source_kind: PriceSourceKind,
    pub scope: PriceScope,
    #[serde(default)]
    pub provider_key: Option<String>,
    pub model_id: String,
    pub rates: PriceRates,
    #[serde(default)]
    pub fetched_at: Option<i64>,
    #[serde(default)]
    pub as_of: Option<String>,
    #[serde(default)]
    pub valid_from: Option<i64>,
    #[serde(default)]
    pub valid_until: Option<i64>,
    #[serde(default)]
    pub attribution: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManualPriceImport {
    pub provider: String,
    pub model: String,
    pub input_price_per_million: f64,
    pub output_price_per_million: f64,
    #[serde(default)]
    pub cache_read_price_per_million: Option<f64>,
    #[serde(default)]
    pub cache_write_price_per_million: Option<f64>,
    #[serde(default)]
    pub reasoning_price_per_million: Option<f64>,
    #[serde(default)]
    pub valid_from: Option<i64>,
    #[serde(default)]
    pub valid_until: Option<i64>,
    #[serde(default)]
    pub attribution: Option<String>,
}

impl ManualPriceImport {
    pub fn observation(self, fetched_at: i64) -> PriceObservation {
        PriceObservation {
            source: "manual-overrides".to_owned(),
            source_kind: PriceSourceKind::Manual,
            scope: PriceScope::RuntimeProvider,
            provider_key: Some(self.provider.trim().to_ascii_lowercase()),
            model_id: self.model,
            rates: PriceRates {
                input_price_per_million: Some(self.input_price_per_million),
                output_price_per_million: Some(self.output_price_per_million),
                cache_read_price_per_million: self.cache_read_price_per_million,
                cache_write_price_per_million: self.cache_write_price_per_million,
                reasoning_price_per_million: self.reasoning_price_per_million,
                ..PriceRates::default()
            },
            fetched_at: Some(fetched_at),
            as_of: None,
            valid_from: self.valid_from,
            valid_until: self.valid_until,
            attribution: self.attribution,
        }
    }
}

impl PriceObservation {
    pub fn validate(&self) -> Result<(), String> {
        if self.source.trim().is_empty() || self.source.len() > 128 {
            return Err("pricing source must be 1-128 characters".to_owned());
        }
        if self.model_id.trim().is_empty() || self.model_id.len() > 512 {
            return Err("pricing model ID must be 1-512 characters".to_owned());
        }
        if self
            .provider_key
            .as_ref()
            .is_some_and(|key| key.trim().is_empty() || key.len() > 128)
        {
            return Err(format!(
                "pricing provider key for '{}' is invalid",
                self.model_id
            ));
        }
        if self
            .valid_from
            .zip(self.valid_until)
            .is_some_and(|(from, until)| from >= until)
        {
            return Err(format!(
                "pricing validity window for '{}' is invalid",
                self.model_id
            ));
        }
        self.rates.validate(&self.model_id)
    }

    pub fn is_valid_at(&self, now: i64) -> bool {
        self.valid_from.is_none_or(|from| now >= from)
            && self.valid_until.is_none_or(|until| now < until)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EffectivePrice {
    pub input_price_per_million: f64,
    pub output_price_per_million: f64,
    pub source: String,
    pub source_kind: PriceSourceKind,
    pub scope: PriceScope,
    pub provider_key: Option<String>,
    pub model_id: String,
    pub fetched_at: Option<i64>,
    pub valid_from: Option<i64>,
    pub valid_until: Option<i64>,
    pub estimated: bool,
}

impl EffectivePrice {
    pub fn from_observation(observation: &PriceObservation, estimated: bool) -> Option<Self> {
        Some(Self {
            input_price_per_million: observation.rates.input_price_per_million?,
            output_price_per_million: observation.rates.output_price_per_million?,
            source: observation.source.clone(),
            source_kind: observation.source_kind,
            scope: observation.scope,
            provider_key: observation.provider_key.clone(),
            model_id: observation.model_id.clone(),
            fetched_at: observation.fetched_at,
            valid_from: observation.valid_from,
            valid_until: observation.valid_until,
            estimated,
        })
    }
}

pub fn normalize_price_id(id: &str) -> String {
    id.trim().to_ascii_lowercase()
}

fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.parse::<f64>().ok())
}

fn cost_number(cost: &Value, key: &str) -> Option<f64> {
    number(cost.get(key)?)
}

/// Parse the provider-specific pricing records from models.dev's api.json.
/// The endpoint is intentionally parsed as a dynamic object so added metadata
/// fields do not break refreshes.
pub fn parse_models_dev(body: &Value, fetched_at: i64) -> Result<Vec<PriceObservation>, String> {
    let providers = body
        .as_object()
        .ok_or_else(|| "models.dev response must be a provider object".to_owned())?;
    let mut observations = Vec::new();
    for (provider_key, provider) in providers {
        let models = provider
            .get("models")
            .and_then(Value::as_object)
            .ok_or_else(|| format!("models.dev provider '{provider_key}' lacks models"))?;
        for (model_id, model) in models {
            let Some(cost) = model.get("cost") else {
                continue;
            };
            let rates = PriceRates {
                input_price_per_million: cost_number(cost, "input"),
                output_price_per_million: cost_number(cost, "output"),
                cache_read_price_per_million: cost_number(cost, "cache_read"),
                cache_write_price_per_million: cost_number(cost, "cache_write"),
                reasoning_price_per_million: cost_number(cost, "reasoning"),
                input_audio_price_per_million: cost_number(cost, "input_audio"),
                output_audio_price_per_million: cost_number(cost, "output_audio"),
                request_price: cost_number(cost, "request"),
                modifiers: cost
                    .as_object()
                    .map(|object| {
                        object
                            .iter()
                            .filter(|(key, _)| *key == "tiers" || key.starts_with("context_"))
                            .map(|(key, value)| (key.clone(), value.clone()))
                            .collect()
                    })
                    .unwrap_or_default(),
            };
            let observation = PriceObservation {
                source: "models.dev".to_owned(),
                source_kind: PriceSourceKind::ModelsDev,
                scope: PriceScope::ProviderProfile,
                provider_key: Some(provider_key.clone()),
                model_id: model_id.clone(),
                rates,
                fetched_at: Some(fetched_at),
                as_of: model
                    .get("last_updated")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                valid_from: None,
                valid_until: None,
                attribution: Some("Models.dev (https://models.dev/)".to_owned()),
            };
            observation.validate()?;
            observations.push(observation);
        }
    }
    Ok(observations)
}

pub fn fetch_models_dev() -> Result<Vec<PriceObservation>, String> {
    let fetched_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_secs();
    let fetched_at = i64::try_from(fetched_at).map_err(|error| error.to_string())?;
    let body: Value = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .user_agent(concat!("model-gateway/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| error.to_string())?
        .get("https://models.dev/api.json")
        .header("Accept", "application/json")
        .send()
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?
        .json()
        .map_err(|error| error.to_string())?;
    parse_models_dev(&body, fetched_at)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{PriceObservation, PriceRates, PriceScope, PriceSourceKind, parse_models_dev};

    #[test]
    fn models_dev_parser_preserves_provider_scope_and_zero_prices() {
        let observations = parse_models_dev(
            &json!({
                "opencode-go": {"models": {
                    "mimo-v2-pro": {"cost": {"input": 1.0, "output": 3.0}, "last_updated": "2026-07-01"}
                }},
                "kilo": {"models": {
                    "step-3.7-flash": {"cost": {"input": 0, "output": 0}}
                }}
            }),
            100,
        )
        .expect("models.dev pricing");
        assert_eq!(observations.len(), 2);
        assert_eq!(observations[0].provider_key.as_deref(), Some("kilo"));
        assert_eq!(observations[1].provider_key.as_deref(), Some("opencode-go"));
        assert_eq!(observations[0].rates.input_price_per_million, Some(0.0));
    }

    #[test]
    fn incomplete_rates_are_not_effective() {
        let observation = PriceObservation {
            source: "fixture".to_owned(),
            source_kind: PriceSourceKind::ModelsDev,
            scope: PriceScope::ProviderProfile,
            provider_key: Some("fixture".to_owned()),
            model_id: "model".to_owned(),
            rates: PriceRates {
                input_price_per_million: Some(1.0),
                ..PriceRates::default()
            },
            fetched_at: Some(100),
            as_of: None,
            valid_from: None,
            valid_until: None,
            attribution: None,
        };
        assert!(super::EffectivePrice::from_observation(&observation, true).is_none());
    }
}
