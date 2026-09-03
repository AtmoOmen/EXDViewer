FROM rust:alpine AS builder
USER root
WORKDIR /app
# reqwest is on rustls and the lock carries no openssl-sys, so no OpenSSL headers are needed.
RUN apk add --no-cache musl-dev trunk \
 && rustup target add wasm32-unknown-unknown

COPY . .
RUN cargo build --bin web --release --features trunk_assets

FROM alpine AS runtime
WORKDIR /app
COPY --from=builder /app/target/release/web web
COPY --from=builder /app/target/release/static static
CMD ["./web"]
