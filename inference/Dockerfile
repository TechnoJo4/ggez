FROM docker.io/library/rust@sha256:0cce0a5e0e8ba67b455257a3a02a1d99005f382748789d6464460028810f1627 AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo test --locked && cargo build --release --locked

FROM scratch
COPY --from=build /src/target/release/inference /inference
USER 65532:65532
ENTRYPOINT ["/inference"]
