FROM rust:1.85-slim AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
# Create dummy source for dependency caching
RUN mkdir src && echo "fn main() {}" > src/main.rs && \
    mkdir -p src/bin && echo "fn main() {}" > src/bin/simulate.rs
RUN cargo build --release 2>/dev/null || true
# Now copy real source and build
COPY src ./src
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates jq && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/rotastellar-agent /usr/local/bin/
COPY constellation/ /etc/rotastellar/
RUN chmod +x /etc/rotastellar/entrypoint.sh
ENTRYPOINT ["/etc/rotastellar/entrypoint.sh"]
