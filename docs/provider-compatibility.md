# Provider Compatibility

This matrix describes the provider profiles implemented in this repository. It
is an implementation and test-status reference, not a guarantee that a remote
provider is available or that an account has quota.

Every implemented profile uses the OpenAI Chat Completions wire format. The
gateway does not currently implement native provider protocols. This matrix
covers inference/model providers only; search, browser automation, image
generation, TTS, and transcription are outside the gateway.

| Provider | Config key | Authentication / endpoint | Status |
| --- | --- | --- | --- |
| Custom endpoint | `custom` | Local or HTTPS OpenAI-compatible endpoint; optional secret | Configurable and contract-tested with deterministic local fixtures |
| CLIProxyAPI | `cli-proxy` | Loopback OpenAI-compatible endpoint with generated bearer key; upstream Claude/Codex OAuth | Optional pinned sidecar; setup, status, and configuration are contract-tested |
| OpenRouter | `openrouter` | HTTPS OpenAI-compatible endpoint; API key | Built-in; catalog authentication is contract-tested |
| Ollama | `ollama` | Local OpenAI-compatible endpoint; no key by default | Built-in |
| LM Studio | `lmstudio` | Local OpenAI-compatible endpoint; optional key | Built-in |
| OpenAI API | `openai-api` | HTTPS OpenAI-compatible endpoint; API key | Built-in; catalog authentication is contract-tested |
| Anthropic | `anthropic` | HTTPS OpenAI-compatible endpoint; API key | Configuration-only profile; no catalog probe is attempted |
| DeepSeek | `deepseek` | HTTPS OpenAI-compatible endpoint; API key | Built-in optional paid profile |
| Fireworks AI | `fireworks` | HTTPS OpenAI-compatible endpoint; API key | Built-in optional paid profile |
| Z.AI / GLM | `zai` | HTTPS OpenAI-compatible endpoint; API key | Built-in optional paid profile |
| Google Gemini | `google-gemini` | Gemini OpenAI compatibility endpoint; API key | Built-in |
| Kilo Code | `kilocode` | HTTPS OpenAI-compatible endpoint; API key | Built-in |
| OpenCode Zen | `opencode-zen` | HTTPS OpenAI-compatible endpoint; API key | Built-in |
| OpenCode Go | `opencode-go` | HTTPS OpenAI-compatible endpoint; API key | Built-in optional subscription profile |
| Mistral AI | `mistral` | HTTPS OpenAI-compatible endpoint; API key | Built-in |
| Nous Portal | `nous-portal` | HTTPS OpenAI-compatible endpoint; API key | Built-in |
| NVIDIA NIM | `nvidia-nim` | HTTPS OpenAI-compatible endpoint; API key | Built-in |
| Groq | `groq` | HTTPS OpenAI-compatible endpoint; API key | Built-in |
| OrcaRouter | `orcarouter` | HTTPS OpenAI-compatible endpoint; API key | Built-in optional paid profile |
| Ollama Cloud | `ollama-cloud` | HTTPS OpenAI-compatible endpoint; API key | Built-in |
| SiliconFlow | `silicon-flow` | HTTPS OpenAI-compatible endpoint; API key | Built-in |

Status meanings:

- **Built-in**: the setup wizard can create the provider entry.
- **Contract-tested**: deterministic local/in-process tests cover the gateway
  wire behavior.
- **Configuration-only**: the profile can be configured, but the gateway does
  not probe a provider catalog automatically.
- **Live OAuth**: requires a user-controlled account and is intentionally not
  exercised in CI.

Provider-specific model availability, pricing, quotas, and policy terms remain
the responsibility of the provider. Refresh catalogs and inspect
`/v1/providers` before routing traffic. See [providers.md](providers.md) for
billing classification and CLIProxyAPI setup.
