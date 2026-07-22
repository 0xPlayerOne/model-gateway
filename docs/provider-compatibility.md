# Provider Compatibility

The compatibility target is Hermes Agent `v0.19.0` / release `v2026.7.20`,
pinned in smoke tests to commit
`3ef6bbd201263d354fd83ec55b3c306ded2eb72a`.
This matrix covers inference/model providers only. Hermes tool services such as
web search, browser automation, image generation, TTS, and transcription are
not gateway providers.

| Provider group | Canonical Hermes IDs | Wire/auth family | Gateway status |
| --- | --- | --- | --- |
| Custom endpoint | `custom` | OpenAI Chat / configured secret | Gateway contract-tested; Hermes model discovery and tool-capable non-streaming request live-tested with deterministic local provider |
| OpenRouter | `openrouter` | OpenAI Chat / API key | Built-in profile; authenticated validation contract-tested |
| Ollama | custom endpoint | OpenAI Chat / local | Built-in profile; profile and OpenAI wire contract-tested |
| LM Studio | `lmstudio` | OpenAI Chat / optional local key | Built-in profile; profile and OpenAI wire contract-tested |
| OpenAI API | `openai-api` | OpenAI Chat / API key | Built-in profile; OpenAI-wire bearer catalog contract-tested |
| Fireworks, Novita, z.ai | `fireworks`, `novita`, `zai` | OpenAI-compatible / API key | Built-in profiles; OpenAI-wire bearer catalog contract-tested |
| DeepSeek | `deepseek` | OpenAI-compatible / API key | Built-in profile; OpenAI-wire bearer catalog contract-tested |
| Kimi, MiniMax, Alibaba | `kimi-coding`, `minimax`, `alibaba` and regional variants | OpenAI-compatible or native variant / API key | Planned profiles/adapters |
| Arcee, GMI, DeepSeek, StepFun, Upstage | `arcee`, `gmi`, `deepseek`, `stepfun`, `upstage` | OpenAI-compatible / API key | Planned profiles |
| Kilo Code, OpenCode Zen/Go, Nous API | `kilocode`, `opencode-zen`, `opencode-go`, `nous` | OpenAI-compatible / API key | Planned profiles |
| Hugging Face, NVIDIA, Ollama Cloud | `huggingface`, `nvidia`, `ollama-cloud` | OpenAI-compatible / API key | Planned profiles |
| Anthropic | `anthropic` | Anthropic Messages / API or OAuth | Planned native adapter |
| Google Gemini | `gemini` | Gemini API / API key | Planned native adapter |
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
