# free-model-gateway

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

## Development

```bash
cargo test
cargo run -- --help
```

## License

This project is licensed under the GNU Affero General Public License v3.0 only
(AGPL-3.0-only). Anyone may use, study, fork, and modify it under that license.
Attribution and source-sharing obligations apply, including when a modified
gateway is offered over a network. The AGPL does not require payment, because a
mandatory commercial-use fee would not be an open-source license. Proprietary
commercial terms would require a separate license from the copyright holders.

See `LICENSE` and `NOTICE`.
