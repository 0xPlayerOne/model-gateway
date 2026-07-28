use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityConfidence {
    SourceExact,
    CanonicalReference,
}

impl IdentityConfidence {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SourceExact => "source_exact",
            Self::CanonicalReference => "canonical_reference",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentityEntityRecord {
    pub id: String,
    pub creator: Option<String>,
    pub family: Option<String>,
    pub version: Option<String>,
    pub variant: Option<String>,
    pub release_date: Option<String>,
    pub hugging_face_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentityAliasRecord {
    pub source: String,
    pub provider_key: String,
    pub provider_model_id: String,
    pub entity_id: String,
    pub confidence: IdentityConfidence,
    pub provenance_url: String,
    pub observed_at: i64,
}

#[derive(Debug, Clone)]
pub struct IdentityImport {
    pub source: String,
    pub attribution: String,
    pub entities: Vec<IdentityEntityRecord>,
    pub aliases: Vec<IdentityAliasRecord>,
}

fn now_seconds() -> Result<i64, String> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_secs();
    i64::try_from(seconds).map_err(|error| error.to_string())
}

fn normalized_reference(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

fn source_entity_id(source: &str, provider: &str, model: &str) -> String {
    format!(
        "source:{source}:{}:{}",
        normalized_reference(provider),
        normalized_reference(model)
    )
}

fn hugging_face_entity_id(hugging_face_id: &str) -> String {
    format!("hf:{}", normalized_reference(hugging_face_id))
}

fn creator_from_id(id: &str) -> Option<String> {
    id.split_once('/')
        .map(|(creator, _)| creator.trim_start_matches('~').to_ascii_lowercase())
        .filter(|creator| !creator.is_empty())
}

pub fn parse_models_dev_identities(
    body: &Value,
    observed_at: i64,
) -> Result<IdentityImport, String> {
    let providers = body
        .as_object()
        .ok_or_else(|| "models.dev response must be a provider object".to_owned())?;
    let mut entities = BTreeMap::<String, IdentityEntityRecord>::new();
    let mut aliases = Vec::new();
    for (provider_key, provider) in providers {
        let models = provider
            .get("models")
            .and_then(Value::as_object)
            .ok_or_else(|| format!("models.dev provider '{provider_key}' lacks models"))?;
        for (model_key, model) in models {
            let model_id = model.get("id").and_then(Value::as_str).unwrap_or(model_key);
            let hf_reference = (provider_key == "huggingface" && model_id.contains('/'))
                .then(|| model_id.to_owned());
            let entity_id = hf_reference
                .as_deref()
                .map(hugging_face_entity_id)
                .unwrap_or_else(|| source_entity_id("models.dev", provider_key, model_id));
            entities
                .entry(entity_id.clone())
                .or_insert_with(|| IdentityEntityRecord {
                    id: entity_id.clone(),
                    creator: creator_from_id(model_id),
                    family: model
                        .get("family")
                        .and_then(Value::as_str)
                        .filter(|family| !family.is_empty())
                        .map(ToOwned::to_owned),
                    version: None,
                    variant: None,
                    release_date: model
                        .get("release_date")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned),
                    hugging_face_id: hf_reference.clone(),
                });
            aliases.push(IdentityAliasRecord {
                source: "models.dev".to_owned(),
                provider_key: provider_key.clone(),
                provider_model_id: model_id.to_owned(),
                entity_id,
                confidence: if hf_reference.is_some() {
                    IdentityConfidence::CanonicalReference
                } else {
                    IdentityConfidence::SourceExact
                },
                provenance_url: "https://models.dev/".to_owned(),
                observed_at,
            });
        }
    }
    Ok(IdentityImport {
        source: "models.dev".to_owned(),
        attribution: "Models.dev (https://models.dev/)".to_owned(),
        entities: entities.into_values().collect(),
        aliases,
    })
}

pub fn parse_openrouter_identities(
    body: &Value,
    observed_at: i64,
) -> Result<IdentityImport, String> {
    let models = body
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| "OpenRouter response must contain a data array".to_owned())?;
    let mut entities = BTreeMap::<String, IdentityEntityRecord>::new();
    let mut aliases = Vec::new();
    for model in models {
        let model_id = model
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| "OpenRouter model lacks id".to_owned())?;
        let hugging_face_id = model
            .get("hugging_face_id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(ToOwned::to_owned);
        let canonical_slug = model
            .get("canonical_slug")
            .and_then(Value::as_str)
            .filter(|slug| !slug.is_empty());
        let entity_id = hugging_face_id
            .as_deref()
            .map(hugging_face_entity_id)
            .unwrap_or_else(|| {
                source_entity_id(
                    "openrouter",
                    "openrouter",
                    canonical_slug.unwrap_or(model_id),
                )
            });
        entities
            .entry(entity_id.clone())
            .or_insert_with(|| IdentityEntityRecord {
                id: entity_id.clone(),
                creator: creator_from_id(model_id),
                family: model
                    .pointer("/architecture/tokenizer")
                    .and_then(Value::as_str)
                    .map(|family| family.to_ascii_lowercase()),
                version: canonical_slug.map(ToOwned::to_owned),
                variant: None,
                release_date: None,
                hugging_face_id: hugging_face_id.clone(),
            });
        aliases.push(IdentityAliasRecord {
            source: "openrouter".to_owned(),
            provider_key: "openrouter".to_owned(),
            provider_model_id: model_id.to_owned(),
            entity_id,
            confidence: if hugging_face_id.is_some() {
                IdentityConfidence::CanonicalReference
            } else {
                IdentityConfidence::SourceExact
            },
            provenance_url: "https://openrouter.ai/api/v1/models".to_owned(),
            observed_at,
        });
    }
    Ok(IdentityImport {
        source: "openrouter".to_owned(),
        attribution: "OpenRouter public models API".to_owned(),
        entities: entities.into_values().collect(),
        aliases,
    })
}

pub fn parse_models_dev_canonical_identities(
    body: &Value,
    observed_at: i64,
) -> Result<IdentityImport, String> {
    let models = body
        .as_object()
        .ok_or_else(|| "models.dev canonical response must be a model object".to_owned())?;
    let mut entities = BTreeMap::<String, IdentityEntityRecord>::new();
    let mut aliases = Vec::new();
    for (model_key, model) in models {
        let model_id = model.get("id").and_then(Value::as_str).unwrap_or(model_key);
        let hugging_face_id = model
            .get("weights")
            .and_then(Value::as_array)
            .and_then(|weights| {
                weights.iter().find_map(|weight| {
                    let url = weight.get("url")?.as_str()?;
                    url.strip_prefix("https://huggingface.co/")
                        .map(|id| id.trim_end_matches('/').to_owned())
                })
            });
        let entity_id = hugging_face_id
            .as_deref()
            .map(hugging_face_entity_id)
            .unwrap_or_else(|| format!("canonical:{}", normalized_reference(model_id)));
        entities
            .entry(entity_id.clone())
            .or_insert_with(|| IdentityEntityRecord {
                id: entity_id.clone(),
                creator: creator_from_id(model_id),
                family: model
                    .get("family")
                    .and_then(Value::as_str)
                    .filter(|family| !family.is_empty())
                    .map(ToOwned::to_owned),
                version: Some(model_id.to_owned()),
                variant: None,
                release_date: model
                    .get("release_date")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                hugging_face_id: hugging_face_id.clone(),
            });
        aliases.push(IdentityAliasRecord {
            source: "models.dev-canonical".to_owned(),
            provider_key: "canonical".to_owned(),
            provider_model_id: model_id.to_owned(),
            entity_id,
            confidence: IdentityConfidence::CanonicalReference,
            provenance_url: "https://models.dev/models.json".to_owned(),
            observed_at,
        });
    }
    Ok(IdentityImport {
        source: "models.dev-canonical".to_owned(),
        attribution: "Models.dev canonical model registry".to_owned(),
        entities: entities.into_values().collect(),
        aliases,
    })
}

fn fetch_json(url: &str) -> Result<Value, String> {
    Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent(concat!("model-gateway/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| error.to_string())?
        .get(url)
        .header("Accept", "application/json")
        .send()
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?
        .json()
        .map_err(|error| error.to_string())
}

pub fn fetch_identity_sources() -> Result<Vec<IdentityImport>, String> {
    let observed_at = now_seconds()?;
    let models_dev = fetch_json("https://models.dev/api.json")?;
    let canonical = fetch_json("https://models.dev/models.json")?;
    let openrouter = fetch_json("https://openrouter.ai/api/v1/models")?;
    let mut models_dev = parse_models_dev_identities(&models_dev, observed_at)?;
    let openrouter = parse_openrouter_identities(&openrouter, observed_at)?;
    let canonical = parse_models_dev_canonical_identities(&canonical, observed_at)?;
    merge_canonical_references(&mut models_dev, &openrouter);
    Ok(vec![models_dev, canonical, openrouter])
}

fn merge_canonical_references(models_dev: &mut IdentityImport, openrouter: &IdentityImport) {
    let entities = openrouter
        .entities
        .iter()
        .map(|entity| (entity.id.clone(), entity.clone()))
        .collect::<BTreeMap<_, _>>();
    let openrouter_ids = openrouter
        .aliases
        .iter()
        .map(|alias| {
            (
                normalized_reference(&alias.provider_model_id),
                alias.entity_id.clone(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut imported_entities = models_dev
        .entities
        .iter()
        .map(|entity| (entity.id.clone(), entity.clone()))
        .collect::<BTreeMap<_, _>>();
    for alias in &mut models_dev.aliases {
        let entity_id = if alias.provider_key == "openrouter" {
            openrouter_ids
                .get(&normalized_reference(&alias.provider_model_id))
                .cloned()
        } else if alias.provider_model_id.contains('/') {
            let candidate = hugging_face_entity_id(&alias.provider_model_id);
            entities.contains_key(&candidate).then_some(candidate)
        } else {
            None
        };
        let Some(entity_id) = entity_id else {
            continue;
        };
        if let Some(entity) = entities.get(&entity_id) {
            imported_entities.insert(entity_id.clone(), entity.clone());
        }
        alias.entity_id = entity_id;
        alias.confidence = IdentityConfidence::CanonicalReference;
        alias.provenance_url = "https://models.dev/ + OpenRouter hugging_face_id".to_owned();
    }
    models_dev.entities = imported_entities.into_values().collect();
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        IdentityConfidence, merge_canonical_references, parse_models_dev_canonical_identities,
        parse_models_dev_identities, parse_openrouter_identities,
    };

    #[test]
    fn models_dev_preserves_provider_scope_and_hugging_face_entities() {
        let import = parse_models_dev_identities(
            &json!({
                "opencode-go": {"models": {
                    "mimo-v2.5": {"id":"mimo-v2.5","family":"mimo-v2.5","release_date":"2026-04-22"}
                }},
                "huggingface": {"models": {
                    "XiaomiMiMo/MiMo-V2.5": {"family":"mimo","release_date":"2026-04-22"}
                }}
            }),
            100,
        )
        .expect("models.dev identities");
        assert_eq!(import.aliases.len(), 2);
        let hf = import
            .aliases
            .iter()
            .find(|alias| alias.provider_key == "huggingface")
            .expect("HF alias");
        assert_eq!(hf.entity_id, "hf:xiaomimimo/mimo-v2.5");
        assert_eq!(hf.confidence, IdentityConfidence::CanonicalReference);
    }

    #[test]
    fn openrouter_uses_hugging_face_id_as_canonical_entity() {
        let import = parse_openrouter_identities(
            &json!({"data":[{
                "id":"xiaomi/mimo-v2.5",
                "canonical_slug":"xiaomi/mimo-v2.5-20260422",
                "hugging_face_id":"XiaomiMiMo/MiMo-V2.5",
                "architecture":{"tokenizer":"Other"}
            }]}),
            100,
        )
        .expect("OpenRouter identities");
        assert_eq!(import.entities[0].id, "hf:xiaomimimo/mimo-v2.5");
        assert_eq!(import.aliases[0].provider_model_id, "xiaomi/mimo-v2.5");
        assert_eq!(
            import.aliases[0].confidence,
            IdentityConfidence::CanonicalReference
        );
    }

    #[test]
    fn models_dev_hugging_face_ids_join_openrouter_entities() {
        let mut models_dev = parse_models_dev_identities(
            &json!({"deepinfra":{"models":{
                "XiaomiMiMo/MiMo-V2.5":{"family":"mimo","release_date":"2026-04-22"}
            }}}),
            100,
        )
        .expect("models.dev");
        let openrouter = parse_openrouter_identities(
            &json!({"data":[{
                "id":"xiaomi/mimo-v2.5",
                "hugging_face_id":"XiaomiMiMo/MiMo-V2.5"
            }]}),
            100,
        )
        .expect("OpenRouter");
        merge_canonical_references(&mut models_dev, &openrouter);
        assert_eq!(models_dev.aliases[0].entity_id, "hf:xiaomimimo/mimo-v2.5");
        assert_eq!(
            models_dev.aliases[0].confidence,
            IdentityConfidence::CanonicalReference
        );
    }

    #[test]
    fn canonical_models_registry_preserves_ids_and_weight_references() {
        let import = parse_models_dev_canonical_identities(
            &json!({
                "nvidia/nemotron-3-ultra-550b-a55b": {
                    "id":"nvidia/nemotron-3-ultra-550b-a55b",
                    "family":"nemotron",
                    "release_date":"2026-06-04",
                    "weights":[{"label":"Hugging Face","url":"https://huggingface.co/nvidia/NVIDIA-Nemotron-3-Ultra-550B-A55B-BF16"}]
                },
                "poolside/laguna-s-2.1": {
                    "family":"laguna",
                    "release_date":"2026-04-07"
                }
            }),
            100,
        )
        .expect("canonical identities");
        assert_eq!(import.aliases.len(), 2);
        assert!(
            import
                .entities
                .iter()
                .any(|entity| { entity.id == "hf:nvidia/nvidia-nemotron-3-ultra-550b-a55b-bf16" })
        );
        assert!(
            import
                .entities
                .iter()
                .any(|entity| entity.id == "canonical:poolside/laguna-s-2.1")
        );
    }
}
