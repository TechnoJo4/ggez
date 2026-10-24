FROM docker.io/library/rust@sha256:0cce0a5e0e8ba67b455257a3a02a1d99005f382748789d6464460028810f1627 AS build
WORKDIR /src/connect-proxy
RUN apk add --no-cache nix python3 py3-pip nodejs npm
COPY git-policy /src/git-policy
COPY host-runtime /src/host-runtime
COPY connect-proxy/Cargo.toml connect-proxy/Cargo.lock ./
COPY connect-proxy/src ./src
RUN NIX_TEST_REQUIRED=1 PACKAGE_TEST_REQUIRED=1 cargo test --locked && cargo build --release --locked

FROM scratch
COPY --from=build /src/connect-proxy/target/release/connect-proxy /connect-proxy
USER 65532:65532
ENTRYPOINT ["/connect-proxy"]
