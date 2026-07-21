# model-gateway

Local Rust model gateway for routing OpenAI-compatible clients to configured
model providers.

The project is being built incrementally. The initial repository commit is a
clean Rust package; gateway configuration, secure local credentials, provider
adapters, Docker, and Hermes compatibility are added in focused commits.

## Development

```bash
cargo test
cargo run
```

Licensed under either `MIT` or `Apache-2.0` at your option.
