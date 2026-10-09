use std::{collections::HashSet, ffi::OsString, path::PathBuf, time::Duration};

pub const HELP: &str = "guest-bridge
  --guest-listener-vsock-port PORT       port the guest accepts vsock connections on
  --host-firecracker-vsock-uds PATH      Firecracker's base UDS, without a _PORT suffix
  --host-bridge-listener-uds PATH        host client listener bridged to the guest
  --host-bridge-socket-mode OCTAL        socket permissions (default 600)
  --max-connections N                    concurrent sessions (default 64)
  --handshake-timeout-seconds N          connect + CONNECT/OK deadline (default 10)
  --idle-timeout-seconds N               no traffic in either direction (default 3600)

host -> guest: dial Firecracker's base UDS and send CONNECT PORT.
the listener path must not exist; its parent directory must already exist.
SIGINT/SIGTERM close sessions and remove the bridge's own socket.";

#[derive(Clone, Debug)]
pub struct Config {
    pub guest_port: u32,
    pub firecracker_socket: PathBuf,
    pub bridge_socket: PathBuf,
    pub socket_mode: u32,
    pub max_connections: usize,
    pub handshake_timeout: Duration,
    pub idle_timeout: Duration,
}

impl Config {
    pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Self, String> {
        let mut args = args.into_iter();
        let mut seen = HashSet::new();
        let mut port = None;
        let mut firecracker_socket = None;
        let mut bridge_socket = None;
        let mut socket_mode = 0o600;
        let mut max_connections = 64;
        let mut handshake_timeout = Duration::from_secs(10);
        let mut idle_timeout = Duration::from_secs(3600);

        while let Some(flag) = args.next() {
            let flag = flag.into_string().map_err(|_| "invalid argument name")?;
            if !seen.insert(flag.clone()) {
                return Err(format!("duplicate argument: {flag}"));
            }
            let value = args
                .next()
                .ok_or_else(|| format!("missing value: {flag}"))?;
            let number = |max| -> Result<u64, String> {
                let n = value.to_str().and_then(|s| s.parse::<u64>().ok());
                n.filter(|n| *n > 0 && *n <= max)
                    .ok_or_else(|| format!("{flag} must be in 1..={max}"))
            };
            match flag.as_str() {
                "--guest-listener-vsock-port" => port = Some(number(u32::MAX as u64 - 1)? as u32),
                "--host-firecracker-vsock-uds" => firecracker_socket = Some(PathBuf::from(value)),
                "--host-bridge-listener-uds" => bridge_socket = Some(PathBuf::from(value)),
                "--host-bridge-socket-mode" => {
                    socket_mode = value
                        .to_str()
                        .and_then(|s| u32::from_str_radix(s, 8).ok())
                        .filter(|n| *n <= 0o777)
                        .ok_or("socket mode must be octal permissions")?;
                }
                "--max-connections" => max_connections = number(65536)? as usize,
                "--handshake-timeout-seconds" => {
                    handshake_timeout = Duration::from_secs(number(86400)?)
                }
                "--idle-timeout-seconds" => {
                    idle_timeout = Duration::from_secs(number(86400 * 365)?)
                }
                _ => return Err(format!("unknown argument: {flag}")),
            }
        }
        let config = Self {
            guest_port: port.ok_or("missing --guest-listener-vsock-port")?,
            firecracker_socket: firecracker_socket.ok_or("missing --host-firecracker-vsock-uds")?,
            bridge_socket: bridge_socket.ok_or("missing --host-bridge-listener-uds")?,
            socket_mode,
            max_connections,
            handshake_timeout,
            idle_timeout,
        };
        if config.firecracker_socket == config.bridge_socket {
            return Err("firecracker and bridge sockets must be different".into());
        }
        for path in [&config.firecracker_socket, &config.bridge_socket] {
            if !path.is_absolute() || path.as_os_str().is_empty() {
                return Err("socket paths must be absolute".into());
            }
        }
        Ok(config)
    }
}
