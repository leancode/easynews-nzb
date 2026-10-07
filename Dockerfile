# Builds a single, statically linked x86_64 Linux binary (musl libc, no dynamic dependencies)
# and ships it in a minimal, non-root image with nothing else in it.

FROM --platform=linux/amd64 rust:1-alpine AS builder
RUN apk add --no-cache musl-dev build-base cmake perl linux-headers
WORKDIR /build
COPY app/Cargo.toml app/Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs && \
    cargo build --release --target x86_64-unknown-linux-musl && \
    rm -rf src
COPY app/src ./src
COPY app/static ./static
RUN touch src/main.rs && \
    cargo build --release --target x86_64-unknown-linux-musl && \
    cp target/x86_64-unknown-linux-musl/release/easynews-nzb /easynews-nzb

# distroless/static: no shell, no package manager, just CA roots, tzdata and a nonroot user -
# exactly what a fully static binary needs and nothing more.
FROM gcr.io/distroless/static-debian12:nonroot
COPY --from=builder /easynews-nzb /easynews-nzb
EXPOSE 8090
ENTRYPOINT ["/easynews-nzb"]
