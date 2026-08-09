use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Digest;

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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct PricingCoverageSummary {
    pub total: usize,
    pub complete: usize,
    pub incomplete: usize,
    pub cache_read: usize,
    pub cache_write: usize,
}

pub fn summarize_pricing(observations: &[PriceObservation]) -> PricingCoverageSummary {
    let mut summary = PricingCoverageSummary {
        total: observations.len(),
        ..PricingCoverageSummary::default()
    };
    for observation in observations {
        if observation.rates.is_complete() {
            summary.complete += 1;
        } else {
            summary.incomplete += 1;
        }
        if observation.rates.cache_read_price_per_million.is_some() {
            summary.cache_read += 1;
        }
        if observation.rates.cache_write_price_per_million.is_some() {
            summary.cache_write += 1;
        }
    }
    summary
}

/// Deterministic content fingerprint for a set of price observations.
/// Order-insensitive and stable across refreshes, so callers can skip storing
/// a new snapshot when the source content is unchanged while still catching
/// in-place revisions (a changed rate, cache rate, or source as-of changes the
/// fingerprint).
pub fn fingerprint_price_observations(observations: &[PriceObservation]) -> String {
    let mut lines = observations
        .iter()
        .map(|observation| {
            let rates = &observation.rates;
            format!(
                "{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{:?}",
                observation.scope.as_str(),
                observation.provider_key.as_deref().unwrap_or(""),
                observation.model_id,
                fmt_number(rates.input_price_per_million),
                fmt_number(rates.output_price_per_million),
                fmt_number(rates.cache_read_price_per_million),
                fmt_number(rates.cache_write_price_per_million),
                fmt_number(rates.reasoning_price_per_million),
                fmt_number(rates.input_audio_price_per_million),
                fmt_number(rates.output_audio_price_per_million),
                fmt_number(rates.request_price),
                observation.as_of.as_deref().unwrap_or(""),
                observation
                    .valid_from
                    .map_or_else(String::new, |value| value.to_string()),
                observation
                    .valid_until
                    .map_or_else(String::new, |value| value.to_string()),
                rates.modifiers,
            )
        })
        .collect::<Vec<_>>();
    lines.sort();
    let mut digest = sha2::Sha256::new();
    for line in lines {
        digest.update(line.as_bytes());
        digest.update(b"\n");
    }
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(crate) fn fmt_number(value: Option<f64>) -> String {
    match value {
        Some(value) => format!("{value}"),
        None => String::new(),
    }
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
            model_id: self.model.trim().to_owned(),
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
    pub cache_read_price_per_million: Option<f64>,
    pub cache_write_price_per_million: Option<f64>,
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
            cache_read_price_per_million: observation.rates.cache_read_price_per_million,
            cache_write_price_per_million: observation.rates.cache_write_price_per_million,
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

    use super::{
        ManualPriceImport, PriceObservation, PriceRates, PriceScope, PriceSourceKind,
        parse_models_dev, summarize_pricing,
    };

    #[test]
    fn pricing_fingerprint_catches_revisions_and_ignores_order() {
        use super::{fingerprint_price_observations, parse_models_dev};
        let body = |input: f64, output: f64| {
            json!({ "openai": { "models": {
                "gpt-5.6-luna": {"cost": {"input": input, "output": output}, "last_updated": "2026-07-09"},
                "gpt-5.6-sol": {"cost": {"input": 5.0, "output": 30.0}, "last_updated": "2026-07-09"}
            }}})
        };
        let original = parse_models_dev(&body(0.2, 1.2), 100).expect("fixture");
        let mut reordered = original.clone();
        reordered.reverse();
        assert_eq!(
            fingerprint_price_observations(&original),
            fingerprint_price_observations(&reordered),
            "fingerprint must be order-insensitive"
        );
        let revised = parse_models_dev(&body(0.2, 1.2), 200).expect("fixture");
        assert_eq!(
            fingerprint_price_observations(&original),
            fingerprint_price_observations(&revised),
            "unchanged content must not change the fingerprint"
        );
        let price_revision = parse_models_dev(&body(0.14, 0.28), 300).expect("fixture");
        assert_ne!(
            fingerprint_price_observations(&original),
            fingerprint_price_observations(&price_revision),
            "an in-place price revision must change the fingerprint"
        );
        let mut cache_revision = original;
        cache_revision[0].rates.cache_read_price_per_million = Some(0.01);
        assert_ne!(
            fingerprint_price_observations(&revised),
            fingerprint_price_observations(&cache_revision),
            "cache pricing revisions must change the fingerprint"
        );
    }

    #[test]
    fn models_dev_parser_preserves_provider_scope_and_zero_prices() {
        let observations = parse_models_dev(
            &json!({
                "opencode-go": {"models": {
                    "mimo-v2-pro": {"cost": {"input": 1.0, "output": 3.0, "cache_read": 0.25, "cache_write": 4.0}, "last_updated": "2026-07-01"}
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
        assert_eq!(
            observations[1].rates.cache_read_price_per_million,
            Some(0.25)
        );
        assert_eq!(
            observations[1].rates.cache_write_price_per_million,
            Some(4.0)
        );
        assert_eq!(
            summarize_pricing(&observations),
            super::PricingCoverageSummary {
                total: 2,
                complete: 2,
                incomplete: 0,
                cache_read: 1,
                cache_write: 1,
            }
        );
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

    #[test]
    fn models_dev_parser_preserves_extended_rate_fields_and_string_numbers() {
        let observations = parse_models_dev(
            &json!({"provider": {"models": {
                "model": {"cost": {
                    "input": "1.0", "output": "2.0", "cache_read": "0.1",
                    "cache_write": 0.2, "reasoning": 3.0, "input_audio": 4.0,
                    "output_audio": 5.0, "request": "0.01", "tiers": [{"limit": 1000}]
                }}
            }}}),
            100,
        )
        .expect("extended models.dev pricing");
        let rates = &observations[0].rates;
        assert_eq!(rates.input_price_per_million, Some(1.0));
        assert_eq!(rates.cache_read_price_per_million, Some(0.1));
        assert_eq!(rates.cache_write_price_per_million, Some(0.2));
        assert_eq!(rates.reasoning_price_per_million, Some(3.0));
        assert_eq!(rates.input_audio_price_per_million, Some(4.0));
        assert_eq!(rates.output_audio_price_per_million, Some(5.0));
        assert_eq!(rates.request_price, Some(0.01));
        assert!(rates.modifiers.contains_key("tiers"));
    }

    #[test]
    fn manual_import_normalizes_provider_and_preserves_billing_windows() {
        let observation = ManualPriceImport {
            provider: "  Fixture ".to_owned(),
            model: "  model  ".to_owned(),
            input_price_per_million: 1.0,
            output_price_per_million: 2.0,
            cache_read_price_per_million: Some(0.5),
            cache_write_price_per_million: Some(0.75),
            reasoning_price_per_million: Some(3.0),
            valid_from: Some(10),
            valid_until: Some(20),
            attribution: Some("fixture".to_owned()),
        }
        .observation(15);
        assert_eq!(observation.provider_key.as_deref(), Some("fixture"));
        assert_eq!(observation.model_id, "model");
        assert!(observation.is_valid_at(15));
        assert!(!observation.is_valid_at(20));
        assert_eq!(observation.rates.reasoning_price_per_million, Some(3.0));
    }

    #[test]
    fn pricing_validation_rejects_invalid_windows_and_negative_rates() {
        let mut observation = PriceObservation {
            source: "fixture".to_owned(),
            source_kind: PriceSourceKind::Manual,
            scope: PriceScope::RuntimeProvider,
            provider_key: Some("fixture".to_owned()),
            model_id: "model".to_owned(),
            rates: PriceRates {
                input_price_per_million: Some(-1.0),
                ..PriceRates::default()
            },
            fetched_at: Some(100),
            as_of: None,
            valid_from: Some(20),
            valid_until: Some(10),
            attribution: None,
        };
        assert!(observation.validate().is_err());
        observation.rates.input_price_per_million = Some(1.0);
        assert!(observation.validate().is_err());
    }

    #[test]
    fn effective_price_preserves_cache_rates() {
        let observation = PriceObservation {
            source: "models.dev".to_owned(),
            source_kind: PriceSourceKind::ModelsDev,
            scope: PriceScope::ProviderProfile,
            provider_key: Some("fixture".to_owned()),
            model_id: "model".to_owned(),
            rates: PriceRates {
                input_price_per_million: Some(1.0),
                output_price_per_million: Some(2.0),
                cache_read_price_per_million: Some(0.25),
                cache_write_price_per_million: Some(3.0),
                ..PriceRates::default()
            },
            fetched_at: Some(100),
            as_of: None,
            valid_from: None,
            valid_until: None,
            attribution: None,
        };

        let effective =
            super::EffectivePrice::from_observation(&observation, false).expect("complete pricing");
        assert_eq!(effective.cache_read_price_per_million, Some(0.25));
        assert_eq!(effective.cache_write_price_per_million, Some(3.0));
    }

    #[test]
    fn validity_windows_are_half_open() {
        let observation = PriceObservation {
            source: "fixture".to_owned(),
            source_kind: PriceSourceKind::Manual,
            scope: PriceScope::RuntimeProvider,
            provider_key: Some("provider".to_owned()),
            model_id: "model".to_owned(),
            rates: PriceRates {
                input_price_per_million: Some(1.0),
                output_price_per_million: Some(2.0),
                ..PriceRates::default()
            },
            fetched_at: Some(100),
            as_of: None,
            valid_from: Some(10),
            valid_until: Some(20),
            attribution: None,
        };
        assert!(!observation.is_valid_at(9));
        assert!(observation.is_valid_at(10));
        assert!(observation.is_valid_at(19));
        assert!(!observation.is_valid_at(20));
    }

    #[test]
    fn validation_rejects_invalid_windows_and_non_finite_rates() {
        let mut observation = PriceObservation {
            source: "fixture".to_owned(),
            source_kind: PriceSourceKind::Manual,
            scope: PriceScope::RuntimeProvider,
            provider_key: Some("provider".to_owned()),
            model_id: "model".to_owned(),
            rates: PriceRates {
                input_price_per_million: Some(f64::NAN),
                output_price_per_million: Some(2.0),
                ..PriceRates::default()
            },
            fetched_at: Some(100),
            as_of: None,
            valid_from: Some(20),
            valid_until: Some(20),
            attribution: None,
        };
        assert!(observation.validate().is_err());
        observation.valid_from = Some(10);
        observation.valid_until = Some(20);
        assert!(observation.validate().is_err());
        observation.rates.input_price_per_million = Some(1.0);
        assert!(observation.validate().is_ok());
    }

    #[test]
    fn models_dev_parser_rejects_invalid_provider_shapes_and_skips_unpriced_models() {
        assert!(parse_models_dev(&json!([]), 100).is_err());
        assert!(parse_models_dev(&json!({"provider": {}}), 100).is_err());
        let observations = parse_models_dev(
            &json!({"provider": {"models": {
                "unpriced": {"name": "no cost"},
                "priced": {"cost": {"input": 1.0, "output": 2.0}}
            }}}),
            100,
        )
        .expect("models.dev pricing");
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].model_id, "priced");
    }
}
