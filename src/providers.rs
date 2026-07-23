use std::time::Duration;

use reqwest::blocking::Client;

use crate::config::{AdapterKind, ProviderConfig, ProviderProfileId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionCheck {
    OpenAiModels,
    OpenRouter,
    ConfigurationOnly,
}

#[derive(Debug, Clone, Copy)]
pub struct ProfileDefinition {
    pub id: ProviderProfileId,
    pub config_key: &'static str,
    pub display_name: &'static str,
    pub adapter: AdapterKind,
    pub default_secret_name: Option<&'static str>,
    pub native_base_url: &'static str,
    pub docker_base_url: Option<&'static str>,
    pub suggested_model: &'static str,
    pub connection_check: ConnectionCheck,
}

pub const PROFILE_DEFINITIONS: &[ProfileDefinition] = &[
    ProfileDefinition {
        id: ProviderProfileId::Custom,
        config_key: "custom",
        display_name: "Custom OpenAI-compatible",
        adapter: AdapterKind::OpenaiChat,
        default_secret_name: None,
        native_base_url: "http://localhost:8000/v1",
        docker_base_url: Some("http://host.docker.internal:8000/v1"),
        suggested_model: "your-model",
        connection_check: ConnectionCheck::OpenAiModels,
    },
    ProfileDefinition {
        id: ProviderProfileId::OpenRouter,
        config_key: "openrouter",
        display_name: "OpenRouter",
        adapter: AdapterKind::OpenaiChat,
        default_secret_name: Some("OPENROUTER_API_KEY"),
        native_base_url: "https://openrouter.ai/api/v1",
        docker_base_url: None,
        suggested_model: "openai/gpt-4o-mini",
        connection_check: ConnectionCheck::OpenRouter,
    },
    ProfileDefinition {
        id: ProviderProfileId::Ollama,
        config_key: "ollama",
        display_name: "Ollama",
        adapter: AdapterKind::OpenaiChat,
        default_secret_name: None,
        native_base_url: "http://localhost:11434/v1",
        docker_base_url: Some("http://host.docker.internal:11434/v1"),
        suggested_model: "llama3.2",
        connection_check: ConnectionCheck::OpenAiModels,
    },
    ProfileDefinition {
        id: ProviderProfileId::LmStudio,
        config_key: "lmstudio",
        display_name: "LM Studio",
        adapter: AdapterKind::OpenaiChat,
        default_secret_name: None,
        native_base_url: "http://localhost:1234/v1",
        docker_base_url: Some("http://host.docker.internal:1234/v1"),
        suggested_model: "local-model",
        connection_check: ConnectionCheck::OpenAiModels,
    },
    ProfileDefinition {
        id: ProviderProfileId::OpenaiApi,
        config_key: "openai-api",
        display_name: "OpenAI API",
        adapter: AdapterKind::OpenaiChat,
        default_secret_name: Some("OPENAI_API_KEY"),
        native_base_url: "https://api.openai.com/v1",
        docker_base_url: None,
        suggested_model: "gpt-4o-mini",
        connection_check: ConnectionCheck::OpenAiModels,
    },
    ProfileDefinition {
        id: ProviderProfileId::Deepseek,
        config_key: "deepseek",
        display_name: "DeepSeek",
        adapter: AdapterKind::OpenaiChat,
        default_secret_name: Some("DEEPSEEK_API_KEY"),
        native_base_url: "https://api.deepseek.com/v1",
        docker_base_url: None,
        suggested_model: "deepseek-chat",
        connection_check: ConnectionCheck::OpenAiModels,
    },
    ProfileDefinition {
        id: ProviderProfileId::Fireworks,
        config_key: "fireworks",
        display_name: "Fireworks AI",
        adapter: AdapterKind::OpenaiChat,
        default_secret_name: Some("FIREWORKS_API_KEY"),
        native_base_url: "https://api.fireworks.ai/inference/v1",
        docker_base_url: None,
        suggested_model: "accounts/fireworks/models/llama-v3p1-8b-instruct",
        connection_check: ConnectionCheck::OpenAiModels,
    },
    ProfileDefinition {
        id: ProviderProfileId::Novita,
        config_key: "novita",
        display_name: "Novita AI",
        adapter: AdapterKind::OpenaiChat,
        default_secret_name: Some("NOVITA_INFRA_KEY"),
        native_base_url: "https://api.novita.ai/openai/v1",
        docker_base_url: None,
        suggested_model: "meta-llama/llama-3.1-8b-instruct",
        connection_check: ConnectionCheck::OpenAiModels,
    },
    ProfileDefinition {
        id: ProviderProfileId::Zai,
        config_key: "zai",
        display_name: "Z.AI / GLM",
        adapter: AdapterKind::OpenaiChat,
        default_secret_name: Some("ZAI_API_KEY"),
        native_base_url: "https://api.z.ai/api/paas/v4",
        docker_base_url: None,
        suggested_model: "glm-4.5",
        connection_check: ConnectionCheck::OpenAiModels,
    },
    ProfileDefinition {
        id: ProviderProfileId::GoogleGemini,
        config_key: "google-gemini",
        display_name: "Google Gemini (OpenAI compatibility)",
        adapter: AdapterKind::OpenaiChat,
        default_secret_name: Some("GOOGLE_API_KEY"),
        native_base_url: "https://generativelanguage.googleapis.com/v1beta/openai",
        docker_base_url: None,
        suggested_model: "gemini-2.5-flash",
        connection_check: ConnectionCheck::OpenAiModels,
    },
    ProfileDefinition {
        id: ProviderProfileId::KiloCode,
        config_key: "kilocode",
        display_name: "Kilo Code Gateway",
        adapter: AdapterKind::OpenaiChat,
        default_secret_name: Some("KILOCODE_API_KEY"),
        native_base_url: "https://api.kilo.ai/api/gateway",
        docker_base_url: None,
        suggested_model: "anthropic/claude-sonnet-4.5",
        connection_check: ConnectionCheck::OpenAiModels,
    },
    ProfileDefinition {
        id: ProviderProfileId::OpenCode,
        config_key: "opencode-zen",
        display_name: "OpenCode Zen",
        adapter: AdapterKind::OpenaiChat,
        default_secret_name: Some("OPENCODE_API_KEY"),
        native_base_url: "https://opencode.ai/zen/v1",
        docker_base_url: None,
        suggested_model: "qwen3-coder",
        connection_check: ConnectionCheck::OpenAiModels,
    },
    ProfileDefinition {
        id: ProviderProfileId::OpenCodeGo,
        config_key: "opencode-go",
        display_name: "OpenCode Go",
        adapter: AdapterKind::OpenaiChat,
        default_secret_name: Some("OPENCODE_API_KEY"),
        native_base_url: "https://opencode.ai/zen/go/v1",
        docker_base_url: None,
        suggested_model: "kimi-k3",
        connection_check: ConnectionCheck::OpenAiModels,
    },
    ProfileDefinition {
        id: ProviderProfileId::Mistral,
        config_key: "mistral",
        display_name: "Mistral AI",
        adapter: AdapterKind::OpenaiChat,
        default_secret_name: Some("MISTRAL_API_KEY"),
        native_base_url: "https://api.mistral.ai/v1",
        docker_base_url: None,
        suggested_model: "mistral-small-latest",
        connection_check: ConnectionCheck::OpenAiModels,
    },
    ProfileDefinition {
        id: ProviderProfileId::NousPortal,
        config_key: "nous-portal",
        display_name: "Nous Portal",
        adapter: AdapterKind::OpenaiChat,
        default_secret_name: Some("NOUS_PORTAL_API_KEY"),
        native_base_url: "https://inference-api.nousresearch.com/v1",
        docker_base_url: None,
        suggested_model: "hermes-4-405b",
        connection_check: ConnectionCheck::OpenAiModels,
    },
    ProfileDefinition {
        id: ProviderProfileId::NvidiaNim,
        config_key: "nvidia-nim",
        display_name: "NVIDIA NIM",
        adapter: AdapterKind::OpenaiChat,
        default_secret_name: Some("NVIDIA_NIM_API_KEY"),
        native_base_url: "https://integrate.api.nvidia.com/v1",
        docker_base_url: None,
        suggested_model: "nvidia/llama-3.1-nemotron-ultra-253b-v1",
        connection_check: ConnectionCheck::OpenAiModels,
    },
    ProfileDefinition {
        id: ProviderProfileId::Groq,
        config_key: "groq",
        display_name: "Groq",
        adapter: AdapterKind::OpenaiChat,
        default_secret_name: Some("GROQ_API_KEY"),
        native_base_url: "https://api.groq.com/openai/v1",
        docker_base_url: None,
        suggested_model: "llama-3.3-70b-versatile",
        connection_check: ConnectionCheck::OpenAiModels,
    },
    ProfileDefinition {
        id: ProviderProfileId::OrcaRouter,
        config_key: "orcarouter",
        display_name: "OrcaRouter",
        adapter: AdapterKind::OpenaiChat,
        default_secret_name: Some("ORCAROUTER_API_KEY"),
        native_base_url: "https://api.orcarouter.ai/v1",
        docker_base_url: None,
        suggested_model: "auto",
        connection_check: ConnectionCheck::OpenAiModels,
    },
    ProfileDefinition {
        id: ProviderProfileId::OllamaCloud,
        config_key: "ollama-cloud",
        display_name: "Ollama Cloud",
        adapter: AdapterKind::OpenaiChat,
        default_secret_name: Some("OLLAMA_API_KEY"),
        native_base_url: "https://ollama.com/v1",
        docker_base_url: None,
        suggested_model: "qwen3-coder:480b",
        connection_check: ConnectionCheck::OpenAiModels,
    },
    ProfileDefinition {
        id: ProviderProfileId::SiliconFlow,
        config_key: "silicon-flow",
        display_name: "SiliconFlow",
        adapter: AdapterKind::OpenaiChat,
        default_secret_name: Some("SILICON_FLOW_API_KEY"),
        native_base_url: "https://api.siliconflow.com/v1",
        docker_base_url: None,
        suggested_model: "deepseek-ai/DeepSeek-V3",
        connection_check: ConnectionCheck::OpenAiModels,
    },
];

pub type BuiltinProvider = ProviderProfileId;

impl ProviderProfileId {
    pub fn all() -> impl ExactSizeIterator<Item = Self> + Clone {
        PROFILE_DEFINITIONS.iter().map(|definition| definition.id)
    }

    pub fn definition(self) -> &'static ProfileDefinition {
        PROFILE_DEFINITIONS
            .iter()
            .find(|definition| definition.id == self)
            .expect("every provider profile ID must have one definition")
    }

    pub fn from_profile_id(id: Option<ProviderProfileId>) -> Self {
        id.unwrap_or(ProviderProfileId::Custom)
    }

    pub fn config_key(self) -> &'static str {
        self.definition().config_key
    }

    pub fn display_name(self) -> &'static str {
        self.definition().display_name
    }

    pub fn default_base_url(self, docker: bool) -> &'static str {
        if docker {
            self.definition()
                .docker_base_url
                .unwrap_or(self.definition().native_base_url)
        } else {
            self.definition().native_base_url
        }
    }

    pub fn needs_api_key(self) -> bool {
        self.definition().default_secret_name.is_some()
    }

    pub fn suggested_model(self) -> &'static str {
        self.definition().suggested_model
    }

    pub fn config(self, base_url: String, api_key_secret: Option<String>) -> ProviderConfig {
        let allow_insecure_http = base_url.starts_with("http://host.docker.internal");
        ProviderConfig {
            profile: Some(self),
            adapter: self.definition().adapter,
            base_url,
            api_key_secret,
            allow_insecure_http,
            ..ProviderConfig::default()
        }
    }

    pub fn validate_and_fetch_models(
        self,
        provider: &ProviderConfig,
        api_key: Option<&str>,
    ) -> Result<Option<Vec<String>>, String> {
        match self.definition().connection_check {
            ConnectionCheck::OpenRouter => {
                validate_openrouter_key(provider, api_key)?;
                fetch_models(provider, api_key).map(Some)
            }
            ConnectionCheck::OpenAiModels => fetch_models(provider, api_key).map(Some),
            ConnectionCheck::ConfigurationOnly => Ok(None),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CatalogModel {
    pub id: String,
    pub zero_priced: bool,
    pub context_length: Option<u64>,
    pub supports_tools: Option<bool>,
    pub supports_vision: Option<bool>,
    pub supports_structured_output: Option<bool>,
    pub input_price_per_million: Option<f64>,
    pub output_price_per_million: Option<f64>,
}

pub fn prepare_request(
    adapter: AdapterKind,
    request: &mut serde_json::Value,
    model: &str,
) -> Result<(), String> {
    match adapter {
        AdapterKind::OpenaiChat => {
            let object = request
                .as_object_mut()
                .ok_or_else(|| "upstream request must be a JSON object".to_owned())?;
            object.insert(
                "model".to_owned(),
                serde_json::Value::String(model.to_owned()),
            );
            Ok(())
        }
    }
}

fn client(provider: &ProviderConfig) -> Result<Client, String> {
    Client::builder()
        .connect_timeout(Duration::from_secs(provider.connect_timeout_seconds))
        .timeout(Duration::from_secs(
            provider.response_header_timeout_seconds,
        ))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| error.to_string())
}

fn validate_openrouter_key(provider: &ProviderConfig, api_key: Option<&str>) -> Result<(), String> {
    fetch_account_limit(provider, api_key)?
        .map(|_| ())
        .ok_or_else(|| "OpenRouter API key is required".to_owned())
}

#[derive(Debug, Clone, PartialEq)]
pub struct AccountLimit {
    pub limit: Option<f64>,
    pub usage: Option<f64>,
    pub remaining: Option<f64>,
    pub is_free_tier: Option<bool>,
}

pub fn fetch_account_limit(
    provider: &ProviderConfig,
    api_key: Option<&str>,
) -> Result<Option<AccountLimit>, String> {
    if provider.profile != Some(ProviderProfileId::OpenRouter) {
        return Ok(None);
    }
    let api_key = api_key.ok_or_else(|| "OpenRouter API key is required".to_owned())?;
    let endpoint = format!("{}/key", provider.base_url.trim_end_matches('/'));
    let response = client(provider)?
        .get(endpoint)
        .bearer_auth(api_key)
        .header("Accept", "application/json")
        .send()
        .map_err(|error| error.to_string())?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!(
            "OpenRouter rejected the API key with HTTP {status}"
        ));
    }
    let body: serde_json::Value = response.json().map_err(|error| error.to_string())?;
    let data = body.get("data").unwrap_or(&body);
    Ok(Some(AccountLimit {
        limit: number_at(data, "limit"),
        usage: number_at(data, "usage"),
        remaining: number_at(data, "limit_remaining"),
        is_free_tier: data
            .get("is_free_tier")
            .and_then(serde_json::Value::as_bool),
    }))
}

pub fn fetch_models(
    provider: &ProviderConfig,
    api_key: Option<&str>,
) -> Result<Vec<String>, String> {
    Ok(fetch_catalog(provider, api_key)?
        .into_iter()
        .map(|model| model.id)
        .collect())
}

pub fn fetch_catalog(
    provider: &ProviderConfig,
    api_key: Option<&str>,
) -> Result<Vec<CatalogModel>, String> {
    let endpoint = format!("{}/models", provider.base_url.trim_end_matches('/'));
    let mut request = client(provider)?
        .get(endpoint)
        .header("Accept", "application/json");
    if let Some(api_key) = api_key {
        request = request.bearer_auth(api_key);
    }
    for (name, value) in &provider.extra_headers {
        request = request.header(name, value);
    }
    let response = request.send().map_err(|error| error.to_string())?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("provider returned HTTP {status}"));
    }
    let body: serde_json::Value = response.json().map_err(|error| error.to_string())?;
    let items = body
        .get("data")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "provider model response did not contain a data array".to_owned())?;
    Ok(items
        .iter()
        .filter_map(|item| {
            let id = item.get("id").and_then(serde_json::Value::as_str)?;
            if is_specialty_model(id) {
                return None;
            }
            let pricing = item.get("pricing");
            let input = pricing.and_then(|pricing| {
                number_at(pricing, "prompt")
                    .or_else(|| number_at(pricing, "input"))
                    .or_else(|| number_at(pricing, "input_price"))
            });
            let output = pricing.and_then(|pricing| {
                number_at(pricing, "completion")
                    .or_else(|| number_at(pricing, "output"))
                    .or_else(|| number_at(pricing, "output_price"))
            });
            let parameters = item
                .get("supported_parameters")
                .and_then(serde_json::Value::as_array);
            let supports_parameter = |names: &[&str]| {
                parameters.map(|parameters| {
                    parameters.iter().any(|parameter| {
                        parameter
                            .as_str()
                            .is_some_and(|parameter| names.contains(&parameter))
                    })
                })
            };
            let modalities = item
                .get("architecture")
                .and_then(|architecture| architecture.get("input_modalities"))
                .and_then(serde_json::Value::as_array);
            Some(CatalogModel {
                id: id.to_owned(),
                zero_priced: matches!((input, output), (Some(input), Some(output)) if input == 0.0 && output == 0.0),
                context_length: item.get("context_length").and_then(serde_json::Value::as_u64),
                supports_tools: supports_parameter(&["tools", "tool_choice"]),
                supports_vision: modalities.map(|modalities| {
                    modalities
                        .iter()
                        .any(|modality| modality.as_str() == Some("image"))
                }),
                supports_structured_output: supports_parameter(&[
                    "response_format",
                    "structured_outputs",
                ]),
                input_price_per_million: input.map(|price| price * 1_000_000.0),
                output_price_per_million: output.map(|price| price * 1_000_000.0),
            })
        })
        .collect())
}

pub fn is_embedding_model(model: &str) -> bool {
    let normalized = model.to_ascii_lowercase();
    let tokens: Vec<&str> = normalized
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .collect();
    tokens
        .iter()
        .any(|token| matches!(*token, "embed" | "embeddings" | "embedding" | "clip"))
        || normalized.contains("text-embedding")
        || normalized.contains("mistral-embed")
        || normalized.contains("jina-embeddings")
        || normalized.contains("nomic-embed")
        || normalized.contains("nv-embed")
        || normalized.contains("nvclip")
        || normalized.contains("bge-")
        || normalized.contains("gte-")
        || normalized.contains("e5-")
}

fn is_audio_model(model: &str) -> bool {
    let normalized = model.to_ascii_lowercase();
    let tokens: Vec<&str> = normalized
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .collect();
    tokens.iter().any(|token| {
        matches!(
            *token,
            "whisper" | "tts" | "speech" | "audio" | "transcribe" | "voxtral" | "lyria"
        )
    }) || normalized.contains("fish-speech")
        || normalized.contains("cosyvoice")
        || normalized.contains("indextts")
        || normalized.contains("orpheus")
}

fn is_image_gen_model(model: &str) -> bool {
    let normalized = model.to_ascii_lowercase();
    let tokens: Vec<&str> = normalized
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .collect();
    tokens
        .iter()
        .any(|token| matches!(*token, "flux" | "imagen" | "sdxl" | "dalle" | "dall"))
        || normalized.contains("stable-diffusion")
        || normalized.contains("diffusion")
        || normalized.ends_with("-image")
        || normalized.contains("-image-")
        || normalized.contains("-image-preview")
        || (tokens.contains(&"image")
            && !normalized.contains("vl")
            && !normalized.contains("-vision"))
}

fn is_video_gen_model(model: &str) -> bool {
    let normalized = model.to_ascii_lowercase();
    let tokens: Vec<&str> = normalized
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .collect();
    tokens.iter().any(|token| {
        matches!(*token, "veo" | "cosmos")
            || (normalized.contains("wan") && (*token == "v" || token.ends_with('v')))
    })
}

fn is_reranker_model(model: &str) -> bool {
    let normalized = model.to_ascii_lowercase();
    let tokens: Vec<&str> = normalized
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .collect();
    tokens
        .iter()
        .any(|token| matches!(*token, "rerank" | "reranker"))
}

fn is_moderation_model(model: &str) -> bool {
    let normalized = model.to_ascii_lowercase();
    let tokens: Vec<&str> = normalized
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .collect();
    tokens.iter().any(|token| token.starts_with("moderat"))
}

fn is_ocr_model(model: &str) -> bool {
    let normalized = model.to_ascii_lowercase();
    let tokens: Vec<&str> = normalized
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .collect();
    tokens
        .iter()
        .any(|token| *token == "ocr" || token.ends_with("ocr"))
}

fn is_safety_model(model: &str) -> bool {
    let normalized = model.to_ascii_lowercase();
    let tokens: Vec<&str> = normalized
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .collect();
    tokens
        .iter()
        .any(|token| token.ends_with("guard") || matches!(*token, "safety" | "safeguard"))
}

fn is_classifier_model(model: &str) -> bool {
    let normalized = model.to_ascii_lowercase();
    let tokens: Vec<&str> = normalized
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .collect();
    tokens.iter().any(|token| {
        *token == "reward" || *token == "pii" || *token == "detect" || *token == "detector"
    })
}

fn is_retrieval_model(model: &str) -> bool {
    let normalized = model.to_ascii_lowercase();
    let tokens: Vec<&str> = normalized
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .collect();
    tokens
        .iter()
        .any(|token| *token == "parse" || token.starts_with("retriev"))
}

pub fn is_specialty_model(model: &str) -> bool {
    is_embedding_model(model)
        || is_audio_model(model)
        || is_image_gen_model(model)
        || is_video_gen_model(model)
        || is_reranker_model(model)
        || is_moderation_model(model)
        || is_ocr_model(model)
        || is_safety_model(model)
        || is_classifier_model(model)
        || is_retrieval_model(model)
}

fn number_at(value: &serde_json::Value, key: &str) -> Option<f64> {
    let value = value.get(key)?;
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    use super::{
        BuiltinProvider, CatalogModel, PROFILE_DEFINITIONS, is_audio_model, is_classifier_model,
        is_embedding_model, is_image_gen_model, is_retrieval_model, is_safety_model,
        is_specialty_model, is_video_gen_model, number_at,
    };
    use crate::config::AdapterKind;

    #[test]
    fn core_profiles_have_expected_defaults() {
        assert_eq!(BuiltinProvider::all().len(), PROFILE_DEFINITIONS.len());
        assert_eq!(
            BuiltinProvider::Ollama.default_base_url(false),
            "http://localhost:11434/v1"
        );
        assert!(BuiltinProvider::OpenRouter.needs_api_key());
        assert!(!BuiltinProvider::LmStudio.needs_api_key());
        assert_eq!(
            BuiltinProvider::OpenaiApi,
            crate::config::ProviderProfileId::OpenaiApi
        );
    }

    #[test]
    fn secondary_profiles_have_expected_defaults() {
        assert_eq!(
            BuiltinProvider::OllamaCloud.default_base_url(false),
            "https://ollama.com/v1"
        );
        assert!(BuiltinProvider::OllamaCloud.needs_api_key());
        assert_eq!(
            BuiltinProvider::OpenCodeGo.default_base_url(false),
            "https://opencode.ai/zen/go/v1"
        );
        assert_eq!(
            BuiltinProvider::OpenCodeGo.definition().default_secret_name,
            Some("OPENCODE_API_KEY")
        );
    }

    #[test]
    fn optional_profiles_have_expected_defaults() {
        assert_eq!(
            BuiltinProvider::SiliconFlow.default_base_url(false),
            "https://api.siliconflow.com/v1"
        );
        assert!(BuiltinProvider::SiliconFlow.needs_api_key());
    }

    #[test]
    fn catalog_pricing_accepts_numeric_and_string_zeroes() {
        let numeric = serde_json::json!({"input": 0, "output": 0.0});
        let strings = serde_json::json!({"prompt": "0", "completion": "0.000"});
        assert_eq!(number_at(&numeric, "input"), Some(0.0));
        assert_eq!(number_at(&strings, "completion"), Some(0.0));
        let model = CatalogModel {
            id: "fixture".to_owned(),
            zero_priced: true,
            context_length: Some(128_000),
            supports_tools: Some(true),
            supports_vision: Some(false),
            supports_structured_output: Some(true),
            input_price_per_million: None,
            output_price_per_million: None,
        };
        assert!(model.zero_priced);
    }

    #[test]
    fn embedding_model_detection_covers_common_provider_names() {
        for model in [
            "text-embedding-3-small",
            "models/gemini-embedding-001",
            "mistral-embed",
            "nvidia/nv-embedqa-e5-v5",
            "BAAI/bge-large-en-v1.5",
            "thenlper/gte-large",
        ] {
            assert!(
                is_embedding_model(model),
                "expected embedding model: {model}"
            );
        }
        assert!(!is_embedding_model("gemini-2.5-flash"));
        assert!(!is_embedding_model("llama-3.3-70b-versatile"));
    }

    #[test]
    fn specialty_model_detection_covers_audio_image_reranker_and_ocr() {
        for model in [
            // Audio/Speech/TTS
            "whisper-large-v3",
            "models/gemini-2.5-flash-preview-tts",
            "models/gemini-2.5-flash-native-audio-latest",
            "voxtral-mini-tts-2603",
            "voxtral-mini-transcribe-realtime-2602",
            "canopylabs/orpheus-v1-english",
            "fishaudio/fish-speech-1.5",
            "FunAudioLLM/CosyVoice2-0.5B",
            "IndexTeam/IndexTTS-2",
            // Image/Video generation
            "black-forest-labs/FLUX.1-dev",
            "models/imagen-4.0-generate-001",
            "Wan-AI/Wan2.2-T2V-A14B",
            "Wan-AI/Wan2.2-I2V-A14B",
            "Qwen/Qwen-Image",
            "Qwen/Qwen-Image-Edit",
            "Tongyi-MAI/Z-Image-Turbo",
            "models/gemini-3.1-flash-image",
            // Reranker / Moderation / OCR
            "Qwen/Qwen3-Reranker-0.6B",
            "mistral-moderation-2603",
            "deepseek/deepseek-ocr",
            "mistral-ocr-latest",
            "paddlepaddle/paddleocr-vl",
        ] {
            assert!(
                is_specialty_model(model),
                "expected specialty model: {model}"
            );
        }
        // Vision-language models should NOT be filtered (they handle text too)
        for model in [
            "Qwen/Qwen3-VL-30B-A3B-Instruct",
            "meta/llama-3.2-11b-vision-instruct",
            "baidu/ernie-4.5-vl-28b-a3b",
            "qwen/qwen2.5-vl-72b-instruct",
            "gemini-2.5-flash",
            "deepseek-ai/DeepSeek-V3",
        ] {
            assert!(
                !is_specialty_model(model),
                "expected non-specialty model: {model}"
            );
        }
    }

    #[test]
    fn new_specialty_categories_catch_previously_missed_models() {
        // DiffusionGemma — image generation
        assert!(is_image_gen_model("google/diffusiongemma-26b-a4b-it"));
        assert!(is_specialty_model("google/diffusiongemma-26b-a4b-it"));
        // Veo — video generation
        assert!(is_video_gen_model("models/veo-3.1-fast-generate-preview"));
        assert!(is_video_gen_model("models/veo-3.1-generate-preview"));
        assert!(is_video_gen_model("models/veo-3.1-lite-generate-preview"));
        // Cosmos — world model / video understanding
        assert!(is_video_gen_model("nvidia/cosmos-reason2-8b"));
        assert!(is_specialty_model("nvidia/cosmos-reason2-8b"));
        // Lyria — music generation (audio)
        assert!(is_audio_model("google/lyria-3-pro-preview"));
        assert!(is_audio_model("models/lyria-realtime-exp"));
        assert!(is_specialty_model("google/lyria-3-pro-preview"));
        // CLIP — vision-language embedding
        assert!(is_embedding_model("nvidia/nvclip"));
        assert!(is_embedding_model("google/lyria-3-clip-preview"));
        assert!(is_specialty_model("nvidia/nvclip"));
        // Guard / safety models
        assert!(is_safety_model("meta-llama/llama-prompt-guard-2-86m"));
        assert!(is_safety_model("meta-llama/llama-guard-4-12b"));
        assert!(is_safety_model("nvidia/nemotron-3.5-content-safety"));
        assert!(is_safety_model(
            "nvidia/llama-3.1-nemoguard-8b-content-safety"
        ));
        assert!(is_safety_model(
            "nvidia/llama-3.1-nemoguard-8b-topic-control"
        ));
        assert!(is_safety_model("openai/gpt-oss-safeguard-20b"));
        assert!(is_specialty_model("meta-llama/llama-prompt-guard-2-86m"));
        assert!(is_specialty_model(
            "nvidia/llama-3.1-nemoguard-8b-content-safety"
        ));
        assert!(is_specialty_model(
            "nvidia/llama-3.1-nemoguard-8b-topic-control"
        ));
        // Reward / classifier models
        assert!(is_classifier_model("nvidia/nemotron-4-340b-reward"));
        assert!(is_classifier_model("nvidia/gliner-pii"));
        assert!(is_specialty_model("nvidia/nemotron-4-340b-reward"));
        assert!(is_specialty_model("nvidia/gliner-pii"));
        // Retrieval / parse models
        assert!(is_retrieval_model("nvidia/nemoretriever-parse"));
        assert!(is_retrieval_model("nvidia/nemotron-parse"));
        assert!(is_specialty_model("nvidia/nemoretriever-parse"));
        assert!(is_specialty_model("nvidia/nemotron-parse"));
        // Video detector — classifier
        assert!(is_classifier_model("nvidia/ai-synthetic-video-detector"));
        assert!(is_specialty_model("nvidia/ai-synthetic-video-detector"));
        // Negative: general chat models should NOT be caught
        assert!(!is_safety_model("gemini-2.5-flash"));
        assert!(!is_classifier_model("deepseek-v4-flash"));
        assert!(!is_retrieval_model("llama-3.3-70b-versatile"));
    }

    #[test]
    fn docker_profiles_use_explicit_host_gateway_and_insecure_opt_in() {
        let url = BuiltinProvider::Ollama.default_base_url(true);
        assert_eq!(url, "http://host.docker.internal:11434/v1");
        let config = BuiltinProvider::Ollama.config(url.to_owned(), None);
        assert!(config.allow_insecure_http);
        assert_eq!(
            config.profile,
            Some(crate::config::ProviderProfileId::Ollama)
        );
    }

    #[test]
    fn profile_registry_has_unique_stable_keys_and_adapter_dispatch() {
        let mut keys = std::collections::BTreeSet::new();
        for profile in BuiltinProvider::all() {
            assert!(keys.insert(profile.config_key()));
            assert_eq!(profile.definition().adapter, AdapterKind::OpenaiChat);
            assert!(url::Url::parse(profile.default_base_url(false)).is_ok());
            assert_eq!(
                profile.needs_api_key(),
                profile.definition().default_secret_name.is_some()
            );
        }
        assert_eq!(keys.len(), PROFILE_DEFINITIONS.len());
        let mut request = serde_json::json!({"model": "alias", "messages": []});
        super::prepare_request(AdapterKind::OpenaiChat, &mut request, "upstream")
            .expect("prepare request");
        assert_eq!(request["model"], "upstream");
    }

    #[test]
    fn configuration_only_profiles_never_contact_the_network() {
        for profile in BuiltinProvider::all().filter(|profile| {
            profile.definition().connection_check == super::ConnectionCheck::ConfigurationOnly
        }) {
            let provider = profile.config("https://127.0.0.1:1/v1".to_owned(), None);
            assert_eq!(
                profile
                    .validate_and_fetch_models(&provider, Some("fixture"))
                    .expect("configuration-only check"),
                None
            );
        }
    }

    #[test]
    fn openai_wire_profiles_use_bearer_catalog_auth() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("mock bind");
        let address = listener.local_addr().expect("mock address");
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("mock accept");
            let mut request = vec![0; 4096];
            let size = socket.read(&mut request).expect("mock read");
            let request = String::from_utf8_lossy(&request[..size]);
            assert!(request.starts_with("GET /v1/models "));
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("authorization: bearer fixture")
            );
            let body = r#"{"data":[{"id":"fixture-model"}]}"#;
            write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .expect("mock write");
        });
        let provider = BuiltinProvider::OpenaiApi.config(format!("http://{address}/v1"), None);
        let models = BuiltinProvider::OpenaiApi
            .validate_and_fetch_models(&provider, Some("fixture"))
            .expect("catalog models")
            .expect("supported model catalog");
        assert_eq!(models, vec!["fixture-model"]);
        server.join().expect("mock server");
    }

    #[test]
    fn every_catalog_profile_uses_the_zero_credit_models_endpoint() {
        for profile in BuiltinProvider::all().filter(|profile| {
            profile.definition().connection_check == super::ConnectionCheck::OpenAiModels
        }) {
            let listener = TcpListener::bind("127.0.0.1:0").expect("mock bind");
            let address = listener.local_addr().expect("mock address");
            let server = thread::spawn(move || {
                let (mut socket, _) = listener.accept().expect("mock accept");
                let mut request = vec![0; 4096];
                let size = socket.read(&mut request).expect("mock read");
                let request = String::from_utf8_lossy(&request[..size]);
                assert!(request.starts_with("GET /v1/models "));
                let body = r#"{"data":[{"id":"fixture-model"}]}"#;
                write!(
                    socket,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .expect("mock write");
            });
            let provider = profile.config(format!("http://{address}/v1"), None);
            let models = profile
                .validate_and_fetch_models(&provider, Some("fixture"))
                .unwrap_or_else(|error| panic!("{} catalog check: {error}", profile.config_key()))
                .expect("supported model catalog");
            assert_eq!(models, vec!["fixture-model"]);
            server.join().expect("mock server");
        }
    }

    #[test]
    fn openrouter_validates_key_before_catalog_discovery() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("mock bind");
        let address = listener.local_addr().expect("mock address");
        let server = thread::spawn(move || {
            for expected_path in ["/v1/key", "/v1/models"] {
                let (mut socket, _) = listener.accept().expect("mock accept");
                let mut request = vec![0; 4096];
                let size = socket.read(&mut request).expect("mock read");
                let request = String::from_utf8_lossy(&request[..size]);
                assert!(request.starts_with(&format!("GET {expected_path} ")));
                assert!(
                    request
                        .to_ascii_lowercase()
                        .contains("authorization: bearer valid-key")
                );
                let body = if expected_path.ends_with("models") {
                    r#"{"data":[{"id":"fixture-model"}]}"#
                } else {
                    r#"{"data":{"label":"fixture","limit":10,"usage":2,"limit_remaining":8,"is_free_tier":true}}"#
                };
                write!(
                    socket,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .expect("mock write");
            }
        });
        let provider = BuiltinProvider::OpenRouter.config(format!("http://{address}/v1"), None);
        let models = BuiltinProvider::OpenRouter
            .validate_and_fetch_models(&provider, Some("valid-key"))
            .expect("validated models")
            .expect("supported model catalog");
        assert_eq!(models, vec!["fixture-model"]);
        server.join().expect("mock server");
    }

    #[test]
    fn openrouter_rejects_invalid_key_without_catalog_request() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("mock bind");
        let address = listener.local_addr().expect("mock address");
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("mock accept");
            let mut request = vec![0; 4096];
            let _ = socket.read(&mut request).expect("mock read");
            write!(
                socket,
                "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .expect("mock write");
        });
        let provider = BuiltinProvider::OpenRouter.config(format!("http://{address}/v1"), None);
        let error = BuiltinProvider::OpenRouter
            .validate_and_fetch_models(&provider, Some("invalid-key"))
            .expect_err("invalid key");
        assert!(error.contains("401"));
        server.join().expect("mock server");
    }
}
