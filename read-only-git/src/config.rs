use crate::{
    client::Client,
    policy::{Policy, Upstreams},
    server::Gateway,
};
use std::{collections::HashSet, ffi::OsString, fs::File, io::Read, path::PathBuf, time::Duration};

pub const HELP: &str = "read-only-git
  --host-git-listener-uds PATH            listener Firecracker forwards guest requests to
  --host-git-socket-mode OCTAL            socket permissions (default 600)
  --host-upstreams-config PATH           JSON object: upstream name -> HTTPS base URL
  --max-connections N                    concurrent requests (default 64)
  --max-request-body-bytes N              negotiation memory bound (default 8388608)
  --request-timeout-seconds N             entire request including pack (default 3600)

guest -> host: create listener at <Firecracker uds_path>_<guest-dialed port>.
base URLs end in '/'. guest repository URLs start with /NAME/.
only GET info/refs?service=git-upload-pack and POST git-upload-pack are admitted.
redirects stay within the original upstream base and the same read operation.
listener path must not exist. SIGINT/SIGTERM close requests and unlink it.";

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
        if !socket.is_absolute() || !config_file.is_absolute() {
            return Err("host socket and config paths must be absolute".into());
        }
        let mut bytes = Vec::new();
        File::open(config_file)
            .and_then(|file| file.take(1024 * 1024 + 1).read_to_end(&mut bytes))
            .map_err(|_| "could not read upstream config")?;
        if bytes.len() > 1024 * 1024 {
            return Err("upstream config exceeds size bound".into());
        }
        let upstreams: Upstreams = serde_json::from_slice(&bytes)
            .map_err(|_| "upstream config must be a JSON object mapping names to base URLs")?;
        let gateway = Gateway {
            policy: Policy::new(upstreams)?,
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
