# renovate: datasource=docker depName=rust versioning=docker
ARG RUST_VERSION=1.99.1
FROM rust:${RUST_VERSION}-alpine AS builder
WORKDIR /usr/src/myapp
RUN apk add --no-cache build-base
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src && printf 'fn main() {}\n' > src/main.rs && cargo build --release --locked
COPY . .
RUN touch src/main.rs && cargo build --release --locked && mkdir -p /root-layer/usr/bin && cp /usr/src/myapp/target/release/qbittorrent-protonvpn /root-layer/usr/bin/

COPY root/ /root-layer/

# Final Docker Mods layer
FROM scratch
COPY --from=builder /root-layer/ /
