# ---- Builder ----
FROM rust:1.87 AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    libasound2-dev pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY . .

RUN cargo build --release --bin oaat

# ---- Runtime ----
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    libasound2 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /src/target/release/oaat /usr/local/bin/oaat
COPY docker/endpoint.toml /etc/oaat/endpoint.toml

# Control (TCP), Audio (UDP), Clock sync (UDP)
EXPOSE 9740/tcp 9741/udp 9742/udp

ENTRYPOINT ["oaat", "endpoint"]
CMD ["--daemon", "--config", "/etc/oaat/endpoint.toml"]
