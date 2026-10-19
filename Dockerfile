# renovate: datasource=docker depName=rust versioning=docker
ARG RUST_VERSION=1.99.1
FROM rust:${RUST_VERSION}-alpine AS builder
WORKDIR /usr/src/myapp
COPY . .
RUN mkdir -p /root-layer/usr/bin && apk add build-base && cargo build --release && cp /usr/src/myapp/target/release/qbittorrent-protonvpn /root-layer/usr/bin/
 
COPY root/ /root-layer/

# Final Docker Mods layer
FROM scratch
COPY --from=builder /root-layer/ /
