#![forbid(unsafe_code)]

mod client;
pub use client::Client;

use hyper::{HeaderMap, Method, StatusCode, Uri, header::HeaderValue};
use reqwest::Url;
use serde::Deserialize;
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

// Redirect bases are host-approved destinations, never discovered from a guest
// request or trusted merely because an upstream returned a Location header.
pub type Upstreams = BTreeMap<String, UpstreamConfig>;

#[derive(Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum UpstreamConfig {
    Url(String),
    WithRedirects {
        url: String,
        redirect_bases: Vec<String>,
    },
}

pub struct Policy {
    upstreams: HashMap<String, GitPolicy>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    Discover,
    UploadPack,
}

impl Operation {
    pub fn method(self) -> Method {
        match self {
            Self::Discover => Method::GET,
            Self::UploadPack => Method::POST,
        }
    }
    pub fn response_type(self) -> &'static str {
        match self {
            Self::Discover => "application/x-git-upload-pack-advertisement",
            Self::UploadPack => "application/x-git-upload-pack-result",
        }
    }
}

// Only the policy can choose a destination and operation. Request bytes remain
// opaque: git-upload-pack interprets them as read negotiation, never as a push.
pub struct AuthorizedRead {
    bases: Arc<[Url]>,
    url: Url,
    operation: Operation,
    headers: HeaderMap,
}

#[derive(Debug)]
pub struct InvalidRedirect;

pub fn clean_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && segment
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.~".contains(&c))
}

fn operation(method: &Method, path: &str, query: Option<&str>) -> Result<Operation, StatusCode> {
    // Tangled supports @handle and did:plc identifiers as repository segments.
    let segment = |s: &str| {
        clean_segment(s)
            || s.strip_prefix('@').is_some_and(clean_segment)
            || s.strip_prefix("did:plc:").is_some_and(clean_segment)
    };
    if path.len() > 4096 || !path.split('/').all(segment) {
        return Err(StatusCode::BAD_REQUEST);
    }
    if method == Method::GET
        && query == Some("service=git-upload-pack")
        && path
            .strip_suffix("/info/refs")
            .is_some_and(|repo| !repo.is_empty())
    {
        Ok(Operation::Discover)
    } else if method == Method::POST
        // Tangled's upload-pack redirect has a trailing '?' with no parameters.
        && query.is_none_or(str::is_empty)
        && path
            .strip_suffix("/git-upload-pack")
            .is_some_and(|repo| !repo.is_empty())
    {
        Ok(Operation::UploadPack)
    } else {
        Err(StatusCode::FORBIDDEN)
    }
}

pub struct GitPolicy(Arc<[Url]>);

impl GitPolicy {
    pub fn new(base: &str) -> Result<Self, String> {
        Ok(Self(vec![Self::base(base)?].into()))
    }

    fn base(base: &str) -> Result<Url, String> {
        let url = Url::parse(base).map_err(|_| "invalid upstream base URL")?;
        // Check raw path too: URL parsing would otherwise normalize '..'.
        let raw_path = base
            .split_once("://")
            .and_then(|(_, rest)| rest.split_once('/').map(|(_, path)| path))
            .ok_or("upstream base URL must end in '/'")?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || !base.ends_with('/')
            || !(raw_path.is_empty()
                || raw_path
                    .strip_suffix('/')
                    .is_some_and(|path| path.split('/').all(clean_segment)))
        {
            return Err("upstream base must be HTTPS without credentials, query, escaping, or traversal, ending in '/'".into());
        }
        Ok(url)
    }

    pub fn authorize(
        &self,
        method: &Method,
        uri: &Uri,
        headers: &HeaderMap,
        body_empty: bool,
    ) -> Result<AuthorizedRead, StatusCode> {
        validate_head(method, uri, headers)?;
        let path = uri
            .path()
            .strip_prefix(self.0[0].path())
            .ok_or(StatusCode::FORBIDDEN)?;
        self.authorize_path(method, uri, path, headers, body_empty)
    }
}

impl Policy {
    pub fn new(upstreams: Upstreams) -> Result<Self, String> {
        let mut bases = HashMap::new();
        if upstreams.is_empty() {
            return Err("upstreams registry is empty".into());
        }
        for (name, config) in upstreams {
            if name.len() > 64 || !clean_segment(&name) {
                return Err("invalid upstream name".into());
            }
            let (url, redirects) = match config {
                UpstreamConfig::Url(url) => (url, Vec::new()),
                UpstreamConfig::WithRedirects {
                    url,
                    redirect_bases,
                } => (url, redirect_bases),
            };
            let mut destinations = vec![GitPolicy::base(&url)?];
            for base in redirects {
                destinations.push(GitPolicy::base(&base)?);
            }
            bases.insert(name, GitPolicy(destinations.into()));
        }
        Ok(Self { upstreams: bases })
    }

    #[cfg(feature = "test-upstreams")]
    pub fn for_test(upstreams: &[(&str, &str)]) -> Self {
        Self {
            upstreams: upstreams
                .iter()
                .map(|(name, url)| {
                    (
                        (*name).to_owned(),
                        GitPolicy(vec![Url::parse(url).unwrap()].into()),
                    )
                })
                .collect(),
        }
    }

    #[cfg(feature = "test-upstreams")]
    pub fn with_test_redirects(mut self, name: &str, urls: &[&str]) -> Self {
        let policy = self.upstreams.get_mut(name).unwrap();
        let mut bases = policy.0.to_vec();
        bases.extend(urls.iter().map(|url| Url::parse(url).unwrap()));
        policy.0 = bases.into();
        self
    }

    pub fn authorize(
        &self,
        method: &Method,
        uri: &Uri,
        headers: &HeaderMap,
        body_empty: bool,
    ) -> Result<AuthorizedRead, StatusCode> {
        validate_head(method, uri, headers)?;
        let (name, path) = uri
            .path()
            .strip_prefix('/')
            .and_then(|p| p.split_once('/'))
            .ok_or(StatusCode::NOT_FOUND)?;
        let base = self.upstreams.get(name).ok_or(StatusCode::NOT_FOUND)?;
        base.authorize_path(method, uri, path, headers, body_empty)
    }
}

fn validate_head(method: &Method, uri: &Uri, headers: &HeaderMap) -> Result<(), StatusCode> {
    if method != Method::GET && method != Method::POST {
        return Err(StatusCode::METHOD_NOT_ALLOWED);
    }
    if uri.scheme().is_some()
        || uri.authority().is_some()
        || uri.to_string().len() > 4096
        || headers.contains_key("upgrade")
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    for value in headers.get_all("connection") {
        if !value.to_str().is_ok_and(|s| {
            s.split(',').all(|token| {
                matches!(
                    token.trim().to_ascii_lowercase().as_str(),
                    "close" | "keep-alive"
                )
            })
        }) {
            return Err(StatusCode::BAD_REQUEST);
        }
    }
    Ok(())
}

impl GitPolicy {
    fn authorize_path(
        &self,
        method: &Method,
        uri: &Uri,
        path: &str,
        headers: &HeaderMap,
        body_empty: bool,
    ) -> Result<AuthorizedRead, StatusCode> {
        let operation = operation(method, path, uri.query())?;
        if operation == Operation::Discover
            && (!body_empty
                || headers.contains_key("transfer-encoding")
                || headers.contains_key("content-encoding")
                || headers.get_all("content-length").iter().any(|v| v != "0"))
        {
            return Err(StatusCode::BAD_REQUEST);
        }
        let mut forwarded = HeaderMap::new();
        if headers.get_all("git-protocol").iter().count() > 1 {
            return Err(StatusCode::BAD_REQUEST);
        }
        if let Some(protocol) = headers.get("git-protocol") {
            if !matches!(
                protocol.to_str(),
                Ok("version=0" | "version=1" | "version=2")
            ) {
                return Err(StatusCode::BAD_REQUEST);
            }
            forwarded.insert("git-protocol", protocol.clone());
        }
        if operation == Operation::UploadPack {
            if headers.get_all("content-type").iter().count() != 1
                || headers.get("content-type")
                    != Some(&HeaderValue::from_static(
                        "application/x-git-upload-pack-request",
                    ))
            {
                return Err(StatusCode::UNSUPPORTED_MEDIA_TYPE);
            }
            forwarded.insert(
                "content-type",
                HeaderValue::from_static("application/x-git-upload-pack-request"),
            );
            if headers.get_all("content-encoding").iter().count() > 1 {
                return Err(StatusCode::BAD_REQUEST);
            }
            if let Some(encoding) = headers.get("content-encoding") {
                if !matches!(encoding.to_str(), Ok("gzip" | "identity")) {
                    return Err(StatusCode::UNSUPPORTED_MEDIA_TYPE);
                }
                forwarded.insert("content-encoding", encoding.clone());
            }
        }
        forwarded.insert(
            "accept",
            HeaderValue::from_static(operation.response_type()),
        );
        forwarded.insert("accept-encoding", HeaderValue::from_static("identity"));
        let mut url = self.0[0].clone();
        url.set_path(&format!("{}{path}", self.0[0].path()));
        url.set_query(uri.query());
        Ok(AuthorizedRead {
            bases: self.0.clone(),
            url,
            operation,
            headers: forwarded,
        })
    }
}

impl AuthorizedRead {
    pub fn url(&self) -> &Url {
        &self.url
    }
    pub fn operation(&self) -> Operation {
        self.operation
    }
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }
    // Recheck the read operation on every redirect, including a forge -> knot
    // hop. Never expose a Location to the guest or admit an unconfigured host.
    pub fn redirect(&mut self, location: &str) -> Result<(), InvalidRedirect> {
        let url = self.url.join(location).map_err(|_| InvalidRedirect)?;
        if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
            return Err(InvalidRedirect);
        }
        let base = self
            .bases
            .iter()
            .find(|base| {
                url.scheme() == base.scheme()
                    && url.host_str() == base.host_str()
                    && url.port_or_known_default() == base.port_or_known_default()
                    && url.path().starts_with(base.path())
            })
            .ok_or(InvalidRedirect)?;
        let path = url
            .path()
            .strip_prefix(base.path())
            .ok_or(InvalidRedirect)?;
        if operation(&self.operation.method(), path, url.query()).map_err(|_| InvalidRedirect)?
            != self.operation
        {
            return Err(InvalidRedirect);
        }
        self.url = url;
        Ok(())
    }
}
