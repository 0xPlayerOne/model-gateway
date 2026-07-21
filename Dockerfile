FROM rust:1.87-bookworm AS builder

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release

FROM debian:bookworm-slim

RUN groupadd --system --gid 10001 model-gateway \
    && useradd --system --uid 10001 --gid 10001 --create-home model-gateway \
    && mkdir -p /app /run/model-gateway/secrets \
    && chown -R model-gateway:model-gateway /app /run/model-gateway

COPY --from=builder /src/target/release/model-gateway /usr/local/bin/model-gateway
COPY gateway.example.toml /app/gateway.example.toml

USER model-gateway
WORKDIR /app
ENV MODEL_GATEWAY_CONFIG=/app/config.toml \
    RUST_LOG=info

ENTRYPOINT ["model-gateway"]
CMD ["serve"]
