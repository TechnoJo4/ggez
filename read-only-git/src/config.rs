use crate::server::Gateway;
use git_policy::{Client, Policy, Upstreams};
use std::{collections::HashSet, ffi::OsString, path::PathBuf, time::Duration};

pub const HELP: &str = "read-only-git
  --host-git-listener-uds PATH            listener Firecracker forwards guest requests to
  --host-git-socket-mode OCTAL            socket permissions (default 600)
  --host-upstreams-config PATH            JSON: name -> HTTPS base or {url, redirect_bases}
  --max-connections N                     concurrent requests (default 64)
  --max-request-body-bytes N              negotiation memory bound (default 8388608)
  --request-timeout-seconds N             entire request including pack (default 3600)

guest -> host: create listener at <Firecracker uds_path>_<guest-dialed port>.
base URLs end in '/'. guest repository URLs start with /NAME/.
only GET info/refs?service=git-upload-pack and POST git-upload-pack are admitted.
redirects stay within the upstream's configured bases and the same read operation.
stale listener sockets are replaced; active sockets and other files are refused.
SIGINT/SIGTERM close requests and unlink the listener socket.";

pub struct Config {
    pub socket: PathBuf,
    pub socket_mode: u32,
    pub max_connections: usize,
    pub deadline: Duration,
    pub gateway: Gateway,
}

impl Config {
    pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Self, String> {
        let mut args = args.into_iter();
        let mut seen = HashSet::new();
        let mut socket = None;
        let mut config_file = None;
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
                "--host-git-listener-uds" => socket = Some(PathBuf::from(value)),
                "--host-upstreams-config" => config_file = Some(PathBuf::from(value)),
                "--host-git-socket-mode" => {
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
        let socket: PathBuf = socket.ok_or("missing --host-git-listener-uds")?;
        let config_file: PathBuf = config_file.ok_or("missing --host-upstreams-config")?;
        if !socket.is_absolute() {
            return Err(format!(
                "--host-git-listener-uds {socket:?}: path must be absolute"
            ));
        }
        let bytes =
            host_runtime::config::read_host_file(&config_file, "upstream config", 1024 * 1024)?;
        let upstreams: Upstreams = serde_json::from_slice(&bytes)
            .map_err(|error| format!("invalid upstream config {config_file:?} (expected names mapped to HTTPS bases or {{url, redirect_bases}}): {error}"))?;
        let gateway = Gateway {
            policy: Policy::new(upstreams)
                .map_err(|error| format!("invalid upstream config {config_file:?}: {error}"))?,
            client: Client::new(deadline).map_err(|error| {
                format!(
                    "could not initialize git HTTPS client: {}",
                    host_runtime::error_chain(&error)
                )
            })?,
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
