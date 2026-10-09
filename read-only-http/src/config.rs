use crate::{client::Client, policy::Policy, server::Gateway};
use std::{collections::HashSet, ffi::OsString, path::PathBuf, time::Duration};

pub const HELP: &str = "read-only-http
  --host-gateway-listener-uds PATH        listener Firecracker forwards guest requests to
  --host-gateway-socket-mode OCTAL        socket permissions (default 600)
  --upstream-origin HTTPS_ORIGIN          fixed upstream; no path or credentials
  --read-path-pattern PATTERN             allowed path; repeatable, required
  --read-query-key KEY                    allowed query key; repeatable, none by default
  --max-connections N                     concurrent requests (default 64)
  --request-timeout-seconds N             entire request including body (default 3600)

guest -> host: create listener at <Firecracker uds_path>_<guest-dialed port>.
patterns match the entire path; '*' matches inside one segment, never across '/'.
GET/HEAD only; no bodies or redirects. listener path must not already exist.";

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
        let mut origin = None;
        let mut paths = Vec::new();
        let mut query_keys = Vec::new();
        let mut socket_mode = 0o600;
        let mut max_connections = 64;
        let mut deadline = Duration::from_secs(3600);
        while let Some(flag) = args.next() {
            let flag = flag.into_string().map_err(|_| "invalid argument name")?;
            if !matches!(flag.as_str(), "--read-path-pattern" | "--read-query-key")
                && !seen.insert(flag.clone())
            {
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
                "--host-gateway-listener-uds" => socket = Some(PathBuf::from(value)),
                "--host-gateway-socket-mode" => {
                    socket_mode = u32::from_str_radix(text()?, 8)
                        .ok()
                        .filter(|n| *n <= 0o777)
                        .ok_or("socket mode must be octal permissions")?
                }
                "--upstream-origin" => origin = Some(text()?.to_owned()),
                "--read-path-pattern" => paths.push(text()?.to_owned()),
                "--read-query-key" => query_keys.push(text()?.to_owned()),
                "--max-connections" => max_connections = number(65536)? as usize,
                "--request-timeout-seconds" => deadline = Duration::from_secs(number(86400 * 365)?),
                _ => return Err(format!("unknown argument: {flag}")),
            }
        }
        let socket: PathBuf = socket.ok_or("missing --host-gateway-listener-uds")?;
        if !socket.is_absolute() {
            return Err("gateway socket path must be absolute".into());
        }
        let gateway = Gateway {
            policy: Policy::new(paths, query_keys)?,
            client: Client::new(&origin.ok_or("missing --upstream-origin")?, deadline)?,
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
