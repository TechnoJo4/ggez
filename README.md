# gateways for ez

| codebase | role |
| --- | --- |
| [guest-bridge](guest-bridge/) | host -> guest: firecracker handshake and byte copying |
| [read-only-http](read-only-http/) | guest -> host: approved reads from a fixed HTTP origin |
| [inference](inference/) | guest -> host: one inference policy with named upstream routes |
| [read-only-git](read-only-git/) | guest -> host: named Git upstreams, clone/fetch only |
| [connect-proxy](connect-proxy/) | guest -> host: TLS termination and explicit read policies for HTTPS clients |

## deployment guidelines

blind outbound forwarding is only eligible for entirely read-only upstreams. otherwise expose reviewed read operations through a protocol-aware gateway.

## development guidelines

review surface and code clarity are of the utmost importance.

the only people who'll use this are me and you, and we will both read the code. document only when it can't go or isn't clear in the code. documentation should never duplicate deployment configuration -- "examples" will only confuse both of us when they inevitably drift.

bare names like `listen-port` / `listen-uds` are banned from configuration: it's very easy to fall into a situation where it's unclear what is listening and on what, argument names should fully identify whose socket/port they describe.

gateway-specific files stay with their gateway; shared files belong at the root. code and tests are the primary documentation.
