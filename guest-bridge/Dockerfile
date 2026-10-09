FROM docker.io/library/rust@sha256:0cce0a5e0e8ba67b455257a3a02a1d99005f382748789d6464460028810f1627 AS build
WORKDIR /src/guest-bridge
COPY host-runtime /src/host-runtime
COPY guest-bridge/Cargo.toml guest-bridge/Cargo.lock ./
COPY guest-bridge/src ./src
COPY guest-bridge/tests ./tests
RUN cargo test --locked && cargo build --release --locked

FROM scratch
COPY --from=build /src/guest-bridge/target/release/guest-bridge /guest-bridge
USER 65532:65532
ENTRYPOINT ["/guest-bridge"]
