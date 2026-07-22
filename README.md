# model-gateway

Local Rust model gateway for routing OpenAI-compatible clients to configured
model providers. It is designed for one developer running locally, not as a
hosted service.

## Native quickstart

```bash
cargo run -- setup
cargo run -- serve
```

`setup` stores provider keys in the macOS Keychain or Linux Secret Service and
writes only non-secret routing configuration under
`~/.config/model-gateway/config.toml`. Use `--offline` to skip catalog checks.
Set `MODEL_GATEWAY_SECRET_STORE=file` to explicitly use protected `0700`/`0600`
storage, or `MODEL_GATEWAY_SECRET_STORE=environment` for environment-only
resolution. The gateway never silently falls back when a keychain is
unavailable.

Hermes can use the gateway as a custom endpoint:

```yaml
model:
  provider: custom
  base_url: http://127.0.0.1:11434/v1
  default: <alias-from-setup>
```

## Docker quickstart

Create the ignored state directory, then run the interactive setup container:

```bash
mkdir -p .model-gateway
export MODEL_GATEWAY_UID="$(id -u)"
export MODEL_GATEWAY_GID="$(id -g)"
docker compose --profile setup run --rm setup
docker compose up --build gateway
```

The provider secrets live in a Docker named volume mounted read-only by the
server. The host port is fixed to `127.0.0.1:11434`; do not broaden it without
designing caller authentication. `docker compose down -v` deletes the local
credential volume.

For Ollama or LM Studio on the host, configure the endpoint as
`http://host.docker.internal:<port>/v1` from the setup container. Container
`localhost` is not the host machine.

## Verification

```bash
curl http://127.0.0.1:11434/health/live
curl http://127.0.0.1:11434/v1/models
```

Set `MODEL_GATEWAY_LOG_FORMAT=json` for structured logs. Both text and JSON
formats contain fixed request metadata only; prompts, responses, tools,
credentials, and arbitrary upstream errors are never logged.

## Supported Profiles

The setup wizard uses one declarative registry for Custom, OpenRouter, Ollama,
LM Studio, OpenAI API, DeepSeek, Fireworks AI, Novita AI, Z.AI/GLM, Google
Gemini's OpenAI compatibility endpoint, Kilo Code, OpenCode Zen, Cerebras,
Mistral, Nous Portal, NVIDIA NIM, Groq, and OrcaRouter. The cloud profiles use
OpenAI Chat Completions with bearer secrets. They are contract-tested against
deterministic local fixtures; no provider credential is required for CI.

`gateway.core.example.toml` configures the CORE providers represented by
`.env.example`. Run `./scripts/core-provider-check.sh` for an explicit one-time
connection check with `.env.local`. It sends only documented model-catalog or
key-status GET requests, skips providers without a documented zero-credit
endpoint, never sends a completion, and is intentionally not part of CI.

## v0.2 Limitations

The gateway supports the OpenAI Chat Completions wire protocol only. It has no
caller authentication or public/LAN bind, does not retry ambiguous transport
failures, does not call providers at startup, and has no config hot reload,
native-protocol adapters, or OAuth-managed credentials.

## Development

```bash
cargo test
cargo run -- --help
```

## Installation

Build from source with the pinned toolchain:

```bash
cargo install --locked --path .
```

Tagged releases publish checksummed native archives for Linux x86_64,
macOS Intel, and macOS Apple Silicon, plus a multi-architecture container
image. Pull the container from GitHub Container Registry with the release tag,
then follow the Docker quickstart above. Release publication is tag-gated;
ordinary branches only run the packaging and container dry-runs.

## License

This project is dual-licensed under the MIT License or Apache License 2.0, at
your option. See `LICENSE-MIT` and `LICENSE-APACHE`.
