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
        connection_check: ConnectionCheck::ConfigurationOnly,
    },
    ProfileDefinition {
        id: ProviderProfileId::OpenCode,
        config_key: "opencode",
        display_name: "OpenCode Zen",
        adapter: AdapterKind::OpenaiChat,
        default_secret_name: Some("OPENCODE_API_KEY"),
        native_base_url: "https://opencode.ai/zen/v1",
        docker_base_url: None,
        suggested_model: "qwen3-coder",
        connection_check: ConnectionCheck::OpenAiModels,
    },
    ProfileDefinition {
        id: ProviderProfileId::Cerebras,
        config_key: "cerebras",
        display_name: "Cerebras",
        adapter: AdapterKind::OpenaiChat,
        default_secret_name: Some("CEREBRAS_API_KEY"),
        native_base_url: "https://api.cerebras.ai/v1",
        docker_base_url: None,
        suggested_model: "qwen-3-coder-480b",
        connection_check: ConnectionCheck::ConfigurationOnly,
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
        native_base_url: "https://portal.nousresearch.com/v1",
        docker_base_url: None,
        suggested_model: "hermes-4-405b",
        connection_check: ConnectionCheck::ConfigurationOnly,
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
        id: ProviderProfileId::Cline,
        config_key: "cline",
        display_name: "Cline API",
        adapter: AdapterKind::OpenaiChat,
        default_secret_name: Some("CLINE_API_KEY"),
        native_base_url: "https://api.cline.bot/api/v1",
        docker_base_url: None,
        suggested_model: "anthropic/claude-sonnet-4-6",
        connection_check: ConnectionCheck::ConfigurationOnly,
    },
    ProfileDefinition {
        id: ProviderProfileId::Gitlawb,
        config_key: "gitlawb",
        display_name: "Gitlawb OpenGateway",
        adapter: AdapterKind::OpenaiChat,
        default_secret_name: Some("GITLAWB_API_GIT"),
        native_base_url: "https://opengateway.gitlawb.com/v1",
        docker_base_url: None,
        suggested_model: "mimo-v2.5-pro",
        connection_check: ConnectionCheck::ConfigurationOnly,
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
    let api_key = api_key.ok_or_else(|| "OpenRouter API key is required".to_owned())?;
    let endpoint = format!("{}/auth/key", provider.base_url.trim_end_matches('/'));
    let response = client(provider)?
        .get(endpoint)
        .bearer_auth(api_key)
        .header("Accept", "application/json")
        .send()
        .map_err(|error| error.to_string())?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(format!(
            "OpenRouter rejected the API key with HTTP {}",
            response.status()
        ))
    }
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
            })
        })
        .collect())
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

    use super::{BuiltinProvider, CatalogModel, PROFILE_DEFINITIONS, number_at};
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
        assert_eq!(
            BuiltinProvider::Cline.default_base_url(false),
            "https://api.cline.bot/api/v1"
        );
        assert!(BuiltinProvider::OllamaCloud.needs_api_key());
        assert!(BuiltinProvider::Cline.needs_api_key());
    }

    #[test]
    fn optional_profiles_have_expected_defaults() {
        assert_eq!(
            BuiltinProvider::Gitlawb.default_base_url(false),
            "https://opengateway.gitlawb.com/v1"
        );
        assert_eq!(
            BuiltinProvider::SiliconFlow.default_base_url(false),
            "https://api.siliconflow.com/v1"
        );
        assert_eq!(BuiltinProvider::Gitlawb.suggested_model(), "mimo-v2.5-pro");
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
        };
        assert!(model.zero_priced);
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
            for expected_path in ["/v1/auth/key", "/v1/models"] {
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
                    r#"{"data":{"label":"fixture"}}"#
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
