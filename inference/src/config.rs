use crate::{
    client::{Client, Endpoint, reserved_header},
    policy::{Policy, Protocol, Route},
    server::Gateway,
};
use hyper::{
    HeaderMap,
    header::{HeaderName, HeaderValue},
};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    ffi::OsString,
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

pub const HELP: &str = "inference
  --host-inference-listener-uds PATH     listener Firecracker forwards guest requests to
  --host-inference-socket-mode OCTAL     socket permissions (default 600)
  --host-upstreams-config PATH           JSON registry; see config.rs for its schema
  --max-connections N                    concurrent requests (default 16)
  --max-request-body-bytes N             request memory bound (default 8388608)
  --max-response-body-bytes N            response byte bound (default 16777216)
  --request-timeout-seconds N            request + response deadline (default 3600)

guest -> host: create listener at <Firecracker uds_path>_<guest-dialed port>.
routes: /NAME/v1/chat/completions and /NAME/v1/messages, when configured.
endpoints are required; models is an optional whitelist (omitted allows all).
keys are read from host-only files.
no gateway rate limiting; upstream 429 and Retry-After are forwarded.
JSON responses are validated before forwarding; stream: true forwards bounded SSE chunks.
stale listener sockets are replaced; active sockets and other files are refused.
SIGINT/SIGTERM close requests and unlink the listener socket.";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryFile {
    pub upstreams: BTreeMap<String, UpstreamConfig>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpstreamConfig {
    pub models: Option<Vec<String>>,
    pub completions: Option<EndpointConfig>,
    pub messages: Option<EndpointConfig>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndpointConfig {
    pub url: String,
    pub authentication: Authentication,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub guest_headers: Vec<String>,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Authentication {
    Bearer { key_file: PathBuf },
    ApiKey { key_file: PathBuf },
}

impl EndpointConfig {
    fn load(self, protocol: Protocol) -> Result<Endpoint, String> {
        let (name, prefix, path) = match self.authentication {
            Authentication::Bearer { key_file } => ("authorization", "Bearer ", key_file),
            Authentication::ApiKey { key_file } => ("x-api-key", "", key_file),
        };
        let key = host_runtime::config::read_host_file(&path, "API key file", 16384)?;
        let key = std::str::from_utf8(&key)
            .map_err(|error| format!("API key file {path:?}: not UTF-8: {error}"))?
            .trim();
        if key.is_empty() || !key.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(format!(
                "API key file {path:?}: expected a nonempty key containing only printable ASCII without whitespace (after trimming)"
            ));
        }
        let mut secret = HeaderValue::from_str(&format!("{prefix}{key}")).map_err(|error| {
            format!("API key file {path:?}: invalid authentication header: {error}")
        })?;
        secret.set_sensitive(true);
        let mut headers = HeaderMap::new();
        for (name, value) in self.headers {
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|error| format!("invalid host header name {name:?}: {error}"))?;
            if reserved_header(name.as_str())
                && !matches!(name.as_str(), "anthropic-version" | "anthropic-beta")
            {
                return Err(format!(
                    "reserved host header {name:?}; authentication must use its config field"
                ));
            }
            let value = HeaderValue::from_str(&value)
                .map_err(|error| format!("invalid host header value for {name:?}: {error}"))?;
            headers.insert(name, value);
        }
        headers.insert(name, secret);
        if matches!(protocol, Protocol::Messages) && !headers.contains_key("anthropic-version") {
            headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
        }
        let mut guest_headers = Vec::new();
        for name in self.guest_headers {
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|error| format!("invalid guest header name {name:?}: {error}"))?;
            if reserved_header(name.as_str()) {
                return Err(format!(
                    "reserved guest header {name:?} cannot be forwarded"
                ));
            }
            guest_headers.push(name);
        }
        Endpoint::new(&self.url, headers, guest_headers)
    }
}

impl RegistryFile {
    fn load(self) -> Result<Policy, String> {
        if self.upstreams.is_empty() {
            return Err("upstreams registry is empty".into());
        }
        let mut routes = HashMap::new();
        for (name, upstream) in self.upstreams {
            if name.is_empty()
                || name.len() > 64
                || !name
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c))
            {
                return Err(format!(
                    "upstream {name:?}: names must be 1..=64 ASCII letters, digits, '-' or '_'"
                ));
            }
            if upstream.models.as_ref().is_some_and(|models| {
                models.is_empty()
                    || models
                        .iter()
                        .any(|model| model.is_empty() || model.len() > 256)
            }) {
                return Err(format!(
                    "upstream {name:?}: model whitelist must contain nonempty model names up to 256 bytes"
                ));
            }
            if upstream.completions.is_none() && upstream.messages.is_none() {
                return Err(format!(
                    "upstream {name:?}: needs at least one endpoint (completions or messages)"
                ));
            }
            let models: Option<Arc<HashSet<String>>> = upstream
                .models
                .map(|models| Arc::new(models.into_iter().collect()));
            for (field, endpoint, protocol, suffix) in [
                (
                    "completions",
                    upstream.completions,
                    Protocol::Completions,
                    "v1/chat/completions",
                ),
                (
                    "messages",
                    upstream.messages,
                    Protocol::Messages,
                    "v1/messages",
                ),
            ] {
                if let Some(endpoint) = endpoint {
                    routes.insert(
                        format!("/{name}/{suffix}"),
                        Route {
                            endpoint: Arc::new(endpoint.load(protocol).map_err(|error| {
                                format!("upstream {name:?}, {field} endpoint: {error}")
                            })?),
                            models: models.clone(),
                            protocol,
                        },
                    );
                }
            }
        }
        Ok(Policy::new(routes))
    }
}

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
        let mut max_connections = 16;
        let mut max_body = 8 * 1024 * 1024;
        let mut max_response_body = 16 * 1024 * 1024;
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
                "--host-inference-listener-uds" => socket = Some(PathBuf::from(value)),
                "--host-upstreams-config" => config_file = Some(PathBuf::from(value)),
                "--host-inference-socket-mode" => {
                    socket_mode = u32::from_str_radix(text()?, 8)
                        .ok()
                        .filter(|n| *n <= 0o777)
                        .ok_or("socket mode must be octal permissions")?
                }
                "--max-connections" => max_connections = number(65536)? as usize,
                "--max-request-body-bytes" => max_body = number(256 * 1024 * 1024)? as usize,
                "--max-response-body-bytes" => {
                    max_response_body = number(256 * 1024 * 1024)? as usize
                }
                "--request-timeout-seconds" => deadline = Duration::from_secs(number(86400 * 365)?),
                _ => return Err(format!("unknown argument: {flag}")),
            }
        }
        let socket: PathBuf = socket.ok_or("missing --host-inference-listener-uds")?;
        if !socket.is_absolute() {
            return Err(format!(
                "--host-inference-listener-uds {socket:?}: path must be absolute"
            ));
        }
        let config_file = config_file.ok_or("missing --host-upstreams-config")?;
        let registry: RegistryFile = serde_json::from_slice(&host_runtime::config::read_host_file(
            &config_file,
            "upstream config",
            1024 * 1024,
        )?)
        .map_err(|error| format!("invalid upstream config {config_file:?}: {error}"))?;
        let gateway = Gateway {
            policy: registry
                .load()
                .map_err(|error| format!("upstream config {config_file:?}: {error}"))?,
            client: Client::new(deadline)?,
            max_body,
            max_response_body,
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct Secret(PathBuf);
    impl Secret {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "ggez-inference-key-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .unwrap();
            std::io::Write::write_all(&mut file, b"test-key\n").unwrap();
            Self(path)
        }
    }
    impl Drop for Secret {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn endpoint(key: &Secret) -> serde_json::Value {
        json!({"url":"https://example.invalid/v1/messages","authentication":{"kind":"api-key","key_file":key.0},"guest_headers":["x-opencode-session"]})
    }

    #[test]
    fn registry_loads_named_routes_and_explicit_model_whitelists() {
        let key = Secret::new();
        let registry: RegistryFile = serde_json::from_value(
            json!({"upstreams":{"opencode-go":{"models":["allowed"],"messages":endpoint(&key)}}}),
        )
        .unwrap();
        let policy = registry.load().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("content-type", "application/json".parse().unwrap());
        let body =
            br#"{"model":"allowed","max_tokens":1024,"messages":[{"role":"user","content":"hi"}]}"#;
        assert!(
            policy
                .authorize(
                    &hyper::Method::POST,
                    &"/opencode-go/v1/messages".parse().unwrap(),
                    &headers,
                    body
                )
                .is_ok()
        );
        assert!(
            policy
                .authorize(
                    &hyper::Method::POST,
                    &"/opencode-go/v1/chat/completions".parse().unwrap(),
                    &headers,
                    body
                )
                .is_err()
        );
        let registry: RegistryFile = serde_json::from_value(
            json!({"upstreams":{"opencode-go":{"models":[],"messages":endpoint(&key)}}}),
        )
        .unwrap();
        assert!(registry.load().is_err());
    }

    #[test]
    fn omitted_model_whitelist_allows_models_in_both_protocols() {
        let key = Secret::new();
        let registry: RegistryFile = serde_json::from_value(json!({"upstreams": {
            "provider": {"completions": endpoint(&key), "messages": endpoint(&key)}
        }}))
        .unwrap();
        let policy = registry.load().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("content-type", "application/json".parse().unwrap());
        for path in ["/provider/v1/messages", "/provider/v1/chat/completions"] {
            let uri = path.parse().unwrap();
            let mut body = json!({"model":"new-provider/model", "max_tokens":1024,
                "messages":[{"role":"user","content":"hi"}]});
            assert!(
                policy
                    .authorize(
                        &hyper::Method::POST,
                        &uri,
                        &headers,
                        &serde_json::to_vec(&body).unwrap()
                    )
                    .is_ok()
            );
            body["stream"] = json!(true);
            assert!(
                policy
                    .authorize(
                        &hyper::Method::POST,
                        &uri,
                        &headers,
                        &serde_json::to_vec(&body).unwrap()
                    )
                    .is_ok()
            );
            body["stream"] = json!(false);
            for model in [json!(null), json!(1), json!(""), json!("x".repeat(257))] {
                body["model"] = model;
                assert!(
                    policy
                        .authorize(
                            &hyper::Method::POST,
                            &uri,
                            &headers,
                            &serde_json::to_vec(&body).unwrap()
                        )
                        .is_err()
                );
            }
        }
    }

    #[test]
    fn transport_and_authentication_headers_cannot_be_guest_forwarded() {
        let key = Secret::new();
        for header in [
            "Authorization",
            "X-Api-Key",
            "host",
            "anthropic-beta",
            "connection",
            "content-length",
        ] {
            let mut config = endpoint(&key);
            config["guest_headers"] = json!([header]);
            let config: EndpointConfig = serde_json::from_value(config).unwrap();
            assert!(
                config.load(Protocol::Messages).is_err(),
                "allowed guest header {header}"
            );
        }
    }

    #[test]
    fn registry_and_secret_fail_closed_on_bad_input() {
        let key = Secret::new();
        let mut config = endpoint(&key);
        config["unexpected"] = json!(true);
        assert!(serde_json::from_value::<EndpointConfig>(config).is_err());
        std::fs::write(&key.0, b"bad\nkey").unwrap();
        let config: EndpointConfig = serde_json::from_value(endpoint(&key)).unwrap();
        assert!(config.load(Protocol::Messages).is_err());
        std::fs::write(&key.0, b"").unwrap();
        let config: EndpointConfig = serde_json::from_value(endpoint(&key)).unwrap();
        assert!(config.load(Protocol::Messages).is_err());
    }
}
