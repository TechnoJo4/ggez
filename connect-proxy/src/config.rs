use crate::{
    client::Client,
    policy::{Policy, Upstreams},
    server::Gateway,
    tls,
};
use std::{
    collections::HashSet,
    ffi::OsString,
    path::{Path, PathBuf},
    time::Duration,
};

pub const HELP: &str = "connect-proxy
  --host-proxy-listener-uds PATH         listener Firecracker forwards guest requests to
  --host-proxy-socket-mode OCTAL         socket permissions (default 600)
  --host-upstreams-config PATH           JSON object: DNS hostname -> tagged policy
  --host-ca-certificate PATH             public CA certificate (PEM)
  --host-ca-signing-key PATH             host-only CA key (unencrypted PKCS#8 PEM)
  --max-connections N                    concurrent sessions (default 64)
  --max-request-body-bytes N             Git negotiation bound (default 8388608)
  --request-timeout-seconds N            entire CONNECT/TLS/HTTP session (default 3600)

guest -> host: create listener at <Firecracker uds_path>_<guest-dialed port>.
CONNECT only, to configured DNS hosts on port 443. TLS terminates here; no tunnels.
leaf keys live in memory; certificates last up to one year, capped by CA expiry.
stale listener sockets are replaced; active sockets and other files are refused.
SIGINT/SIGTERM close sessions and unlink the listener socket.";

pub struct Config {
    pub socket: PathBuf,
    pub socket_mode: u32,
    pub max_connections: usize,
    pub deadline: Duration,
    pub gateway: Gateway,
}

fn read(path: &Path, role: &str) -> Result<String, String> {
    let bytes = host_runtime::config::read_host_file(path, role, 1024 * 1024)?;
    String::from_utf8(bytes)
        .map_err(|error| format!("{role} {path:?}: not UTF-8: {}", error.utf8_error()))
}

impl Config {
    pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Self, String> {
        let mut args = args.into_iter();
        let mut seen = HashSet::new();
        let mut socket = None;
        let mut registry = None;
        let mut certificate = None;
        let mut signing_key = None;
        let mut socket_mode = 0o600;
        let mut max_connections = 64;
        let mut max_body = 8 * 1024 * 1024;
        let mut deadline = Duration::from_secs(3600);
        while let Some(flag) = args.next() {
            let flag = flag.into_string().map_err(|_| "invalid argument name")?;
            if !seen.insert(flag.clone()) {
                return Err(format!("duplicate argument: {flag}"));
            }
            let value = args
                .next()
                .ok_or_else(|| format!("missing value: {flag}"))?;
            let text = || {
                value
                    .to_str()
                    .ok_or_else(|| format!("invalid value: {flag}"))
            };
            let number = |max| -> Result<u64, String> {
                text()?
                    .parse::<u64>()
                    .ok()
                    .filter(|n| *n > 0 && *n <= max)
                    .ok_or_else(|| format!("{flag} must be in 1..={max}"))
            };
            match flag.as_str() {
                "--host-proxy-listener-uds" => socket = Some(PathBuf::from(value)),
                "--host-upstreams-config" => registry = Some(PathBuf::from(value)),
                "--host-ca-certificate" => certificate = Some(PathBuf::from(value)),
                "--host-ca-signing-key" => signing_key = Some(PathBuf::from(value)),
                "--host-proxy-socket-mode" => {
                    socket_mode = u32::from_str_radix(text()?, 8)
                        .ok()
                        .filter(|n| *n <= 0o777)
                        .ok_or("socket mode must be octal permissions")?
                }
                "--max-connections" => max_connections = number(65536)? as usize,
                "--max-request-body-bytes" => max_body = number(256 * 1024 * 1024)? as usize,
                "--request-timeout-seconds" => deadline = Duration::from_secs(number(86400 * 365)?),
                _ => return Err(format!("unknown argument: {flag}")),
            }
        }
        let socket: PathBuf = socket.ok_or("missing --host-proxy-listener-uds")?;
        if !socket.is_absolute() {
            return Err(format!(
                "--host-proxy-listener-uds {socket:?}: path must be absolute"
            ));
        }
        let registry = registry.ok_or("missing --host-upstreams-config")?;
        let upstreams: Upstreams =
            serde_json::from_str(&read(&registry, "upstream policy config")?)
                .map_err(|error| format!("invalid upstream policy config {registry:?}: {error}"))?;
        let policy = Policy::new(upstreams)
            .map_err(|error| format!("invalid upstream policy config {registry:?}: {error}"))?;
        let certificate_path = certificate.ok_or("missing --host-ca-certificate")?;
        let signing_key_path = signing_key.ok_or("missing --host-ca-signing-key")?;
        let certificate = read(&certificate_path, "CA certificate")?;
        let signing_key = read(&signing_key_path, "CA signing key")?;
        let acceptors = tls::acceptors(&certificate, &signing_key, policy.hosts())
            .map_err(|error| format!("TLS initialization (CA certificate {certificate_path:?}, CA signing key {signing_key_path:?}): {error}"))?;
        let gateway = Gateway {
            policy,
            acceptors,
            client: Client::new(deadline)?,
            max_body,
        };
        Ok(Self {
            socket,
            socket_mode,
            max_connections,
            deadline,
            gateway,
        })
    }
}
