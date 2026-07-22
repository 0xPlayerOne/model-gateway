use std::time::Duration;

use reqwest::blocking::Client;

use crate::config::{AdapterKind, ProviderConfig, ProviderProfileId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinProvider {
    Custom,
    OpenRouter,
    Ollama,
    LmStudio,
    OpenaiApi,
    Deepseek,
    Fireworks,
    Novita,
    Zai,
}

#[derive(Debug, Clone, Copy)]
pub struct ProfileDefinition {
    pub id: ProviderProfileId,
    pub display_name: &'static str,
    pub adapter: AdapterKind,
    pub api_key_required: bool,
    pub default_secret_name: Option<&'static str>,
    pub native_base_url: &'static str,
    pub docker_base_url: &'static str,
    pub suggested_model: &'static str,
}

impl BuiltinProvider {
    pub fn all() -> &'static [Self] {
        &[
            Self::Custom,
            Self::OpenRouter,
            Self::Ollama,
            Self::LmStudio,
            Self::OpenaiApi,
            Self::Deepseek,
            Self::Fireworks,
            Self::Novita,
            Self::Zai,
        ]
    }

    pub fn definition(self) -> ProfileDefinition {
        match self {
            Self::Custom => ProfileDefinition {
                id: ProviderProfileId::Custom,
                display_name: "Custom OpenAI-compatible",
                adapter: AdapterKind::OpenaiChat,
                api_key_required: false,
                default_secret_name: None,
                native_base_url: "http://localhost:8000/v1",
                docker_base_url: "http://host.docker.internal:8000/v1",
                suggested_model: "your-model",
            },
            Self::OpenRouter => ProfileDefinition {
                id: ProviderProfileId::OpenRouter,
                display_name: "OpenRouter",
                adapter: AdapterKind::OpenaiChat,
                api_key_required: true,
                default_secret_name: Some("OPENROUTER_API_KEY"),
                native_base_url: "https://openrouter.ai/api/v1",
                docker_base_url: "https://openrouter.ai/api/v1",
                suggested_model: "openai/gpt-4o-mini",
            },
            Self::Ollama => ProfileDefinition {
                id: ProviderProfileId::Ollama,
                display_name: "Ollama",
                adapter: AdapterKind::OpenaiChat,
                api_key_required: false,
                default_secret_name: None,
                native_base_url: "http://localhost:11434/v1",
                docker_base_url: "http://host.docker.internal:11434/v1",
                suggested_model: "llama3.2",
            },
            Self::LmStudio => ProfileDefinition {
                id: ProviderProfileId::LmStudio,
                display_name: "LM Studio",
                adapter: AdapterKind::OpenaiChat,
                api_key_required: false,
                default_secret_name: None,
                native_base_url: "http://localhost:1234/v1",
                docker_base_url: "http://host.docker.internal:1234/v1",
                suggested_model: "local-model",
            },
            Self::OpenaiApi => ProfileDefinition {
                id: ProviderProfileId::OpenaiApi,
                display_name: "OpenAI API",
                adapter: AdapterKind::OpenaiChat,
                api_key_required: true,
                default_secret_name: Some("OPENAI_API_KEY"),
                native_base_url: "https://api.openai.com/v1",
                docker_base_url: "https://api.openai.com/v1",
                suggested_model: "gpt-4o-mini",
            },
            Self::Deepseek => ProfileDefinition {
                id: ProviderProfileId::Deepseek,
                display_name: "DeepSeek",
                adapter: AdapterKind::OpenaiChat,
                api_key_required: true,
                default_secret_name: Some("DEEPSEEK_API_KEY"),
                native_base_url: "https://api.deepseek.com/v1",
                docker_base_url: "https://api.deepseek.com/v1",
                suggested_model: "deepseek-chat",
            },
            Self::Fireworks => ProfileDefinition {
                id: ProviderProfileId::Fireworks,
                display_name: "Fireworks AI",
                adapter: AdapterKind::OpenaiChat,
                api_key_required: true,
                default_secret_name: Some("FIREWORKS_API_KEY"),
                native_base_url: "https://api.fireworks.ai/inference/v1",
                docker_base_url: "https://api.fireworks.ai/inference/v1",
                suggested_model: "accounts/fireworks/models/llama-v3p1-8b-instruct",
            },
            Self::Novita => ProfileDefinition {
                id: ProviderProfileId::Novita,
                display_name: "Novita AI",
                adapter: AdapterKind::OpenaiChat,
                api_key_required: true,
                default_secret_name: Some("NOVITA_API_KEY"),
                native_base_url: "https://api.novita.ai/openai/v1",
                docker_base_url: "https://api.novita.ai/openai/v1",
                suggested_model: "meta-llama/llama-3.1-8b-instruct",
            },
            Self::Zai => ProfileDefinition {
                id: ProviderProfileId::Zai,
                display_name: "Z.AI / GLM",
                adapter: AdapterKind::OpenaiChat,
                api_key_required: true,
                default_secret_name: Some("ZAI_API_KEY"),
                native_base_url: "https://api.z.ai/api/paas/v4",
                docker_base_url: "https://api.z.ai/api/paas/v4",
                suggested_model: "glm-4.5",
            },
        }
    }

    pub fn profile_id(self) -> ProviderProfileId {
        self.definition().id
    }

    pub fn from_profile_id(id: Option<ProviderProfileId>) -> Self {
        match id.unwrap_or(ProviderProfileId::Custom) {
            ProviderProfileId::Custom => Self::Custom,
            ProviderProfileId::OpenRouter => Self::OpenRouter,
            ProviderProfileId::Ollama => Self::Ollama,
            ProviderProfileId::LmStudio => Self::LmStudio,
            ProviderProfileId::OpenaiApi => Self::OpenaiApi,
            ProviderProfileId::Deepseek => Self::Deepseek,
            ProviderProfileId::Fireworks => Self::Fireworks,
            ProviderProfileId::Novita => Self::Novita,
            ProviderProfileId::Zai => Self::Zai,
        }
    }

    pub fn config_key(self) -> &'static str {
        match self {
            Self::Custom => "custom",
            Self::OpenRouter => "openrouter",
            Self::Ollama => "ollama",
            Self::LmStudio => "lmstudio",
            Self::OpenaiApi => "openai-api",
            Self::Deepseek => "deepseek",
            Self::Fireworks => "fireworks",
            Self::Novita => "novita",
            Self::Zai => "zai",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Custom => "Custom OpenAI-compatible",
            Self::OpenRouter => "OpenRouter",
            Self::Ollama => "Ollama",
            Self::LmStudio => "LM Studio",
            Self::OpenaiApi => "OpenAI API",
            Self::Deepseek => "DeepSeek",
            Self::Fireworks => "Fireworks AI",
            Self::Novita => "Novita AI",
            Self::Zai => "Z.AI / GLM",
        }
    }

    pub fn default_base_url(self, docker: bool) -> &'static str {
        match (self, docker) {
            (Self::Custom, true) => "http://host.docker.internal:8000/v1",
            (Self::Ollama, true) => "http://host.docker.internal:11434/v1",
            (Self::LmStudio, true) => "http://host.docker.internal:1234/v1",
            (Self::Custom, false) => "http://localhost:8000/v1",
            (Self::OpenRouter, _) => "https://openrouter.ai/api/v1",
            (Self::Ollama, false) => "http://localhost:11434/v1",
            (Self::LmStudio, false) => "http://localhost:1234/v1",
            (Self::OpenaiApi, _) => "https://api.openai.com/v1",
            (Self::Deepseek, _) => "https://api.deepseek.com/v1",
            (Self::Fireworks, _) => "https://api.fireworks.ai/inference/v1",
            (Self::Novita, _) => "https://api.novita.ai/openai/v1",
            (Self::Zai, _) => "https://api.z.ai/api/paas/v4",
        }
    }

    pub fn needs_api_key(self) -> bool {
        self.definition().api_key_required
    }

    pub fn suggested_model(self) -> &'static str {
        match self {
            Self::Custom => "your-model",
            Self::OpenRouter => "openai/gpt-4o-mini",
            Self::Ollama => "llama3.2",
            Self::LmStudio => "local-model",
            Self::OpenaiApi => "gpt-4o-mini",
            Self::Deepseek => "deepseek-chat",
            Self::Fireworks => "accounts/fireworks/models/llama-v3p1-8b-instruct",
            Self::Novita => "meta-llama/llama-3.1-8b-instruct",
            Self::Zai => "glm-4.5",
        }
    }

    pub fn config(self, base_url: String, api_key_secret: Option<String>) -> ProviderConfig {
        let allow_insecure_http = base_url.starts_with("http://host.docker.internal");
        ProviderConfig {
            profile: Some(self.profile_id()),
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
    ) -> Result<Vec<String>, String> {
        if self == Self::OpenRouter {
            validate_openrouter_key(provider, api_key)?;
        }
        fetch_models(provider, api_key)
    }
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
        .filter_map(|item| item.get("id").and_then(serde_json::Value::as_str))
        .map(ToOwned::to_owned)
        .collect())
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    use super::BuiltinProvider;
    use crate::config::AdapterKind;

    #[test]
    fn core_profiles_have_expected_defaults() {
        assert_eq!(BuiltinProvider::all().len(), 9);
        assert_eq!(
            BuiltinProvider::Ollama.default_base_url(false),
            "http://localhost:11434/v1"
        );
        assert!(BuiltinProvider::OpenRouter.needs_api_key());
        assert!(!BuiltinProvider::LmStudio.needs_api_key());
        assert_eq!(
            BuiltinProvider::OpenaiApi.profile_id(),
            crate::config::ProviderProfileId::OpenaiApi
        );
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
        }
        let mut request = serde_json::json!({"model": "alias", "messages": []});
        super::prepare_request(AdapterKind::OpenaiChat, &mut request, "upstream")
            .expect("prepare request");
        assert_eq!(request["model"], "upstream");
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
            .expect("catalog models");
        assert_eq!(models, vec!["fixture-model"]);
        server.join().expect("mock server");
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
            .expect("validated models");
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
