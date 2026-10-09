FROM docker.io/library/rust@sha256:0cce0a5e0e8ba67b455257a3a02a1d99005f382748789d6464460028810f1627 AS build
WORKDIR /src/read-only-http
COPY host-runtime /src/host-runtime
COPY read-only-http/Cargo.toml read-only-http/Cargo.lock ./
COPY read-only-http/src ./src
RUN cargo test --locked && cargo build --release --locked

FROM scratch
COPY --from=build /src/read-only-http/target/release/read-only-http /read-only-http
USER 65532:65532
ENTRYPOINT ["/read-only-http"]
