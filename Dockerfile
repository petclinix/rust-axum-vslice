# ---------------------------------------------------------------------------
# builder: full toolchain, compiles the release binary. Not shipped.
# ---------------------------------------------------------------------------
FROM rust:1-slim AS builder

WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY src ./src

RUN cargo build --release

# ---------------------------------------------------------------------------
# runtime: just the compiled binary + CA certs, no toolchain. Last stage, so
# it's the default `docker build` target — one statically-ish linked binary,
# no JVM/interpreter, no second container for a DB (see PLAN.md §11).
# ---------------------------------------------------------------------------
FROM debian:trixie-slim AS runtime

RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /app/target/release/rust-axum-vslice ./rust-axum-vslice

ENV DATA_DIR=/data
EXPOSE 8080
VOLUME ["/data"]

CMD ["./rust-axum-vslice"]
