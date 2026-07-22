FROM rust:1.87-bookworm AS builder

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release

FROM debian:bookworm-slim

ARG MODEL_GATEWAY_UID=10001
ARG MODEL_GATEWAY_GID=10001

RUN if ! getent group "$MODEL_GATEWAY_GID" >/dev/null; then groupadd --gid "$MODEL_GATEWAY_GID" model-gateway; fi \
    && useradd --uid "$MODEL_GATEWAY_UID" --gid "$MODEL_GATEWAY_GID" --create-home model-gateway \
    && mkdir -p /app/state /run/model-gateway/secrets \
    && chown -R "$MODEL_GATEWAY_UID:$MODEL_GATEWAY_GID" /app /run/model-gateway

COPY --from=builder /src/target/release/model-gateway /usr/local/bin/model-gateway
COPY gateway.example.toml /app/gateway.example.toml
COPY gateway.core.example.toml /app/gateway.core.example.toml
COPY gateway.secondary.example.toml /app/gateway.secondary.example.toml

USER model-gateway
WORKDIR /app
ENV MODEL_GATEWAY_CONFIG=/app/state/config.toml \
    RUST_LOG=info

HEALTHCHECK --interval=10s --timeout=3s --start-period=5s --retries=3 CMD ["model-gateway", "healthcheck"]

ENTRYPOINT ["model-gateway"]
CMD ["serve"]
