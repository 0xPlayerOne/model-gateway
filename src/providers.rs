use std::time::Duration;

use reqwest::blocking::Client;

use crate::config::{AdapterKind, ProviderConfig};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinProvider {
    Custom,
    OpenRouter,
    Ollama,
    LmStudio,
}

impl BuiltinProvider {
    pub fn all() -> &'static [Self] {
        &[Self::Custom, Self::OpenRouter, Self::Ollama, Self::LmStudio]
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Custom => "Custom OpenAI-compatible",
            Self::OpenRouter => "OpenRouter",
            Self::Ollama => "Ollama",
            Self::LmStudio => "LM Studio",
        }
    }

    pub fn default_base_url(self) -> &'static str {
        match self {
            Self::Custom => "http://localhost:8000/v1",
            Self::OpenRouter => "https://openrouter.ai/api/v1",
            Self::Ollama => "http://localhost:11434/v1",
            Self::LmStudio => "http://localhost:1234/v1",
        }
    }

    pub fn needs_api_key(self) -> bool {
        matches!(self, Self::OpenRouter)
    }

    pub fn suggested_model(self) -> &'static str {
        match self {
            Self::Custom => "your-model",
            Self::OpenRouter => "openai/gpt-4o-mini",
            Self::Ollama => "llama3.2",
            Self::LmStudio => "local-model",
        }
    }

    pub fn config(self, base_url: String, api_key_secret: Option<String>) -> ProviderConfig {
        ProviderConfig {
            adapter: AdapterKind::OpenaiChat,
            base_url,
            api_key_secret,
            ..ProviderConfig::default()
        }
    }
}

pub fn fetch_models(
    provider: &ProviderConfig,
    api_key: Option<&str>,
) -> Result<Vec<String>, String> {
    let endpoint = format!("{}/models", provider.base_url.trim_end_matches('/'));
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(provider.connect_timeout_seconds))
        .timeout(Duration::from_secs(
            provider.response_header_timeout_seconds,
        ))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| error.to_string())?;
    let mut request = client.get(endpoint).header("Accept", "application/json");
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
    use super::BuiltinProvider;

    #[test]
    fn core_profiles_have_expected_defaults() {
        assert_eq!(BuiltinProvider::all().len(), 4);
        assert_eq!(
            BuiltinProvider::Ollama.default_base_url(),
            "http://localhost:11434/v1"
        );
        assert!(BuiltinProvider::OpenRouter.needs_api_key());
        assert!(!BuiltinProvider::LmStudio.needs_api_key());
    }
}
