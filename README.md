# ggez

small static host-side gateways for an offline nixos firecracker guest. human review surface comes first.

| codebase | role |
| --- | --- |
| [guest-bridge](guest-bridge/) | host -> guest: firecracker handshake and byte copying |
| [read-only-http](read-only-http/) | guest -> host: approved reads from a fixed HTTP origin |
| [inference](inference/) | guest -> host: one inference policy with named upstream routes |
| [read-only-git](read-only-git/) | guest -> host: named Git upstreams, clone/fetch only |

each folder owns its code, tests, dependencies, and dockerfile. build with that folder as the context. dockerfiles run Linux tests and produce non-root `FROM scratch` images using a pinned rust + musl builder.

`--help` defines each binary's arguments. endpoint arguments identify whose socket/port they describe and its purpose; bare `listen-port` / `listen-uds` names are banned. listener directories must be writable by the container UID (65532 by default). existing socket paths are never replaced.

inference's registry schema lives in [config.rs](inference/src/config.rs). endpoint URLs, per-upstream model whitelists, host key files, and explicitly forwarded guest headers are configured there. completions and messages retain their own JSON wire formats; streaming requests are rejected. newline keepalives start after successful upstream headers, before the complete JSON body. upstream keys handle budgets; 429 and Retry-After pass through without gateway retries or rate limiting.

blind outbound forwarding is only eligible for entirely read-only upstreams. otherwise expose reviewed read operations through a protocol-aware gateway.

git's host registry is a JSON object mapping names to HTTPS base URLs ending in `/`. guest rewrites need the same trailing slash: `url."http://gateway/github/".insteadOf = "https://github.com/"`. only upload-pack discovery and negotiation reach upstream; receive-pack is rejected locally.

gateway-specific files stay with their gateway; shared files belong at the root. code and tests are the primary documentation.
