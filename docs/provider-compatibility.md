# Provider Compatibility

The compatibility target is Hermes Agent `v0.19.0` / release `v2026.7.20`,
pinned in smoke tests to commit
`3ef6bbd201263d354fd83ec55b3c306ded2eb72a`.

Compatibility labels in this document are evidence scopes, not availability or
live-service guarantees. `Gateway contract` means deterministic local fixture
coverage; `Hermes integration` means the pinned Hermes smoke reached the
gateway; `Live-tested` is reserved for a credentialed provider run recorded
without exposing its credential.
This matrix covers inference/model providers only. Hermes tool services such as
web search, browser automation, image generation, TTS, and transcription are
not gateway providers.

| Provider group | Canonical Hermes IDs | Wire/auth family | Gateway status |
| --- | --- | --- | --- |
| Custom endpoint | `custom` | OpenAI Chat / configured secret | Gateway contract-tested; Hermes model discovery and tool-bearing non-streaming request integration-tested with deterministic local provider |
| OpenRouter | `openrouter` | OpenAI Chat / API key | Built-in profile; authenticated validation contract-tested |
| Ollama | custom endpoint | OpenAI Chat / local | Built-in profile; profile and OpenAI wire contract-tested |
| LM Studio | `lmstudio` | OpenAI Chat / optional local key | Built-in profile; profile and OpenAI wire contract-tested |
| OpenAI API | `openai-api` | OpenAI Chat / API key | Built-in profile; OpenAI-wire bearer catalog contract-tested |
| Fireworks, Novita, z.ai | `fireworks`, `novita`, `zai` | OpenAI-compatible / API key | Built-in profiles; OpenAI-wire bearer catalog contract-tested |
| DeepSeek | `deepseek` | OpenAI-compatible / API key | Built-in profile; OpenAI-wire bearer catalog contract-tested |
| Gemini, OpenCode, Mistral, NVIDIA NIM, Groq, OrcaRouter | `google-gemini`, `opencode`, `mistral`, `nvidia-nim`, `groq`, `orcarouter` | OpenAI-compatible / API key | Built-in profiles; zero-credit model catalogs checked on demand |
| Kilo Code, Cerebras, Nous Portal | `kilocode`, `cerebras`, `nous-portal` | OpenAI-compatible / API key | Built-in profiles; configuration-only check because no documented zero-credit endpoint is available |
| Kimi, MiniMax, Alibaba | `kimi-coding`, `minimax`, `alibaba` and regional variants | OpenAI-compatible or native variant / API key | Planned profiles/adapters |
| Arcee, GMI, StepFun, Upstage | `arcee`, `gmi`, `stepfun`, `upstage` | OpenAI-compatible / API key | Planned profiles |
| Hugging Face, Ollama Cloud | `huggingface`, `ollama-cloud` | OpenAI-compatible or native / API key | Planned profiles/adapters |
| Anthropic | `anthropic` | Anthropic Messages / API or OAuth | Planned native adapter |
| Google Gemini native API | `gemini-native` | Gemini API / API key | Planned native adapter; the OpenAI compatibility endpoint is supported |
| Vertex AI | `vertex` | Vertex OpenAI-compatible / OAuth | Planned credential adapter |
| Azure Foundry | `azure-foundry` | OpenAI-compatible / API key or Entra | Planned credential adapter |
| AWS Bedrock | `bedrock` | Bedrock Converse / AWS credentials | Planned native adapter |
| xAI, Copilot, Codex | `xai`, `copilot`, `openai-codex` | Responses/Chat / API or OAuth | Planned adapters |
| Nous Portal, Qwen OAuth, MiniMax OAuth | `nous`, `qwen-oauth`, `minimax-oauth` | Provider-specific / OAuth | Planned OAuth flows |
| GitHub Copilot ACP | `copilot-acp` | Local ACP subprocess | Planned isolated adapter |

Status meanings:

- **Built-in profile**: setup can create the provider entry.
- **Contract-tested**: deterministic local/in-process tests cover the wire contract.
- **Live-tested**: a real credential and provider endpoint have been exercised.
- **Planned**: intentionally not claimed yet.

New Hermes releases require an explicit matrix review because the upstream
plugin registry and provider documentation can diverge.
