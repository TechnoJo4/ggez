# gateways for ez

gateways for a sandboxed agent without a network interface, with access control baked into the logic. [vm setup instructions](https://merkletr.ee/2026/ez)

| codebase | role |
| --- | --- |
| [guest-bridge](guest-bridge/) | host -> guest: firecracker handshake and byte copying |
| [inference](inference/) | guest -> host: one inference policy with named upstream routes |
| [read-only-git](read-only-git/) | guest -> host: named Git upstreams, clone/fetch only |
| [connect-proxy](connect-proxy/) | guest -> host: TLS termination and explicit read policies for HTTPS clients |

## usage

upon request, this repository will be git pulled (through the ssh guest-bridge!), reviewed by a human, then re-deployed.

after a re-deploy, run `nix run . -- /etc/nixos/gateways` (ensuring `./gateways/guest.nix` is part of `nixosSystem.modules`) to update your system with support for new capabilities.

## development guidelines

review surface and code clarity are of the utmost importance.

use a subagent to conduct an audit for every change, use one for an ontology review for every major change.

the only people who'll use this are me and you, and we will both read the code. document only when it can't go or isn't clear in the code. documentation should never duplicate deployment configuration -- "examples" will only confuse both of us when production inevitably drifts away.

blind outbound forwarding is only eligible for entirely read-only upstreams. otherwise expose reviewed read operations through a protocol-aware gateway.

bare names like `listen-port` / `listen-uds` are banned from configuration: it's very easy to fall into a situation where it's unclear what is listening and on what, argument names should fully identify whose socket/port they describe.

all error cases (or at least HTTP 5XXes + equivalents) should log something specific to that error case, alongside any error from a dependency. 4XX-equivalents should either return a clear message to the client or log.

gateway-specific files stay with their gateway; shared files belong at the root. code and tests are the primary documentation.

do not write change-detector tests -- avoid tests for behavior of dependencies, for specific strings in logs, errors, responses, etc.
