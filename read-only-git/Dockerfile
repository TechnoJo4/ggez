FROM docker.io/library/rust@sha256:0cce0a5e0e8ba67b455257a3a02a1d99005f382748789d6464460028810f1627 AS build
WORKDIR /src/read-only-git
RUN apk add --no-cache git git-daemon
COPY git-policy /src/git-policy
COPY host-runtime /src/host-runtime
COPY read-only-git/Cargo.toml read-only-git/Cargo.lock ./
COPY read-only-git/src ./src
RUN cargo test --locked && cargo build --release --locked

FROM scratch
COPY --from=build /src/read-only-git/target/release/read-only-git /read-only-git
USER 65532:65532
ENTRYPOINT ["/read-only-git"]
