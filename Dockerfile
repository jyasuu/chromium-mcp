FROM rust:alpine AS builder

RUN apk add --no-cache musl-dev pkgconf perl python3 make g++

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src/ src/

RUN cargo build --release

FROM alpine:3.20

RUN apk add --no-cache chromium nss nspr

COPY --from=builder /app/target/release/chromium-mcp /usr/local/bin/chromium-mcp

ENV CHROME_PATH=/usr/bin/chromium
ENV CHROME_FLAGS="--disable-gpu --disable-dev-shm-usage --disable-software-rasterizer"

EXPOSE 8787

ENTRYPOINT ["chromium-mcp"]
CMD ["--transport", "http", "--addr", "0.0.0.0:8787"]
