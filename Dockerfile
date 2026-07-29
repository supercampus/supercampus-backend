FROM rust:1.97-bookworm AS builder
WORKDIR /app
COPY . .
RUN cargo build --release -p supercampus-platform-api

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --create-home supercampus
WORKDIR /app
COPY --from=builder /app/target/release/supercampus-platform-api /usr/local/bin/supercampus-platform-api
ENV HTTP_HOST=0.0.0.0
ENV HTTP_PORT=4000
USER supercampus
EXPOSE 4000
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 CMD curl --fail http://127.0.0.1:4000/health || exit 1
ENTRYPOINT ["supercampus-platform-api"]