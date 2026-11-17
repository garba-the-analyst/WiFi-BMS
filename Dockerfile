# Central API image. Build: docker build -t wifi-bms-api .
FROM rust:1.88-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY entitlement entitlement
COPY api api
COPY site-agent site-agent
RUN cargo build --release -p platform-api --bin platform-api

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --no-create-home app
COPY --from=build /src/target/release/platform-api /usr/local/bin/platform-api
USER app
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/platform-api"]
