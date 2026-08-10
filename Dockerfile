FROM rust:1.97.1-bookworm AS builder

WORKDIR /src
COPY Cargo.toml Cargo.lock ./

# Compile the full dependency tree against stub sources first so this layer
# is cached and reused across releases. The real sources are copied below,
# so only the model-gateway crate itself recompiles per release.
RUN mkdir -p src && \
    printf 'fn main() {}\n' > src/main.rs && \
    printf '' > src/lib.rs && \
    cargo build --release --locked || true

COPY src ./src
COPY docs ./docs

# Force recompile of the real crate sources; dependencies are reused from
# the cached dependency layer above.
RUN touch src/main.rs src/lib.rs && \
    cargo build --release --locked

FROM debian:bookworm-slim

ARG MODEL_GATEWAY_UID=10001
ARG MODEL_GATEWAY_GID=10001

RUN if ! getent group "$MODEL_GATEWAY_GID" >/dev/null; then groupadd --gid "$MODEL_GATEWAY_GID" model-gateway; fi \
    && useradd --uid "$MODEL_GATEWAY_UID" --gid "$MODEL_GATEWAY_GID" --create-home model-gateway \
    && mkdir -p /app/state /run/model-gateway/secrets /var/lib/model-gateway \
    && chown -R "$MODEL_GATEWAY_UID:$MODEL_GATEWAY_GID" /app /run/model-gateway /var/lib/model-gateway

COPY --from=builder /src/target/release/model-gateway /usr/local/bin/model-gateway
COPY gateway.example.toml /app/gateway.example.toml
COPY gateway.core.example.toml /app/gateway.core.example.toml
COPY gateway.secondary.example.toml /app/gateway.secondary.example.toml
COPY gateway.optional.example.toml /app/gateway.optional.example.toml

USER model-gateway
WORKDIR /app
ENV MODEL_GATEWAY_CONFIG=/app/state/config.toml \
    MODEL_GATEWAY_STATE_PATH=/var/lib/model-gateway/routing.sqlite3 \
    RUST_LOG=info

HEALTHCHECK --interval=10s --timeout=3s --start-period=5s --retries=3 CMD ["model-gateway", "healthcheck"]

ENTRYPOINT ["model-gateway"]
CMD ["serve"]
