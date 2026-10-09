mod cargo_files;
mod cargo_index;
mod forgejo;
mod github;
mod github_api;
mod github_codeload;
mod gitlab;
mod nix_cache;
mod pypi;
mod pypi_files;
mod sourcehut;

use git_policy::{AuthorizedRead as GitRead, GitPolicy, clean_segment};
use hyper::{HeaderMap, Method, StatusCode, Uri};
use reqwest::Url;
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(tag = "policy", rename_all = "kebab-case", deny_unknown_fields)]
pub enum UpstreamConfig {
    CargoIndex {},
    CargoFiles {},
    Git {},
    Github {},
    GithubApi {},
    GithubCodeload {},
    Forgejo {},
    Gitlab {},
    NixCache {},
    Pypi {},
    PypiFiles {},
    Sourcehut {},
}

impl UpstreamConfig {
    fn allows_head(self) -> bool {
        matches!(
            self,
            Self::NixCache {}
                | Self::Pypi {}
                | Self::PypiFiles {}
                | Self::CargoIndex {}
                | Self::CargoFiles {}
        )
    }

    fn allows_git(self) -> bool {
        matches!(
            self,
            Self::Git {}
                | Self::Github {}
                | Self::Forgejo {}
                | Self::Gitlab {}
                | Self::Sourcehut {}
        )
    }

    fn read(self, uri: &Uri) -> Option<ReadOperation> {
        match self {
            Self::CargoIndex {} => cargo_index::authorize(uri),
            Self::CargoFiles {} => cargo_files::authorize(uri),
            Self::Git {} => None, // All Git authorization is in the shared git-policy crate.
            Self::Github {} => github::authorize(uri),
            Self::GithubApi {} => github_api::authorize(uri),
            Self::GithubCodeload {} => github_codeload::authorize(uri),
            Self::Forgejo {} => forgejo::authorize(uri),
            Self::Gitlab {} => gitlab::authorize(uri),
            Self::NixCache {} => nix_cache::authorize(uri),
            Self::Pypi {} => pypi::authorize(uri),
            Self::PypiFiles {} => pypi_files::authorize(uri),
            Self::Sourcehut {} => sourcehut::authorize(uri),
        }
    }

    fn redirects_to(self, host: &str, next: Self, next_host: &str) -> bool {
        match self {
            Self::Github {} | Self::GithubApi {} | Self::GithubCodeload {} => {
                github::redirects_to(next)
            }
            Self::Forgejo {} => forgejo::redirects_to(host, next, next_host),
            Self::Gitlab {} => gitlab::redirects_to(host, next, next_host),
            Self::Sourcehut {} => sourcehut::redirects_to(host, next, next_host),
            Self::Git {}
            | Self::NixCache {}
            | Self::Pypi {}
            | Self::PypiFiles {}
            | Self::CargoIndex {}
            | Self::CargoFiles {} => false,
        }
    }
}

pub type Upstreams = BTreeMap<String, UpstreamConfig>;

struct Upstream {
    kind: UpstreamConfig,
    git: GitPolicy,
}

pub(crate) struct Policy {
    upstreams: HashMap<String, Upstream>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ReadOperation {
    Cache,
    PackageFile,
    Lookup {
        accept: &'static str,
    },
    Archive {
        kind: UpstreamConfig,
        repo: String,
        rev: String,
    },
}

// Neither transport nor client can invent an authorized target.
pub(crate) enum AuthorizedRequest {
    Git(GitRead),
    Http(AuthorizedHttpRead),
}

pub(crate) struct AuthorizedHttpRead {
    method: Method,
    url: Url,
    headers: HeaderMap,
    kind: UpstreamConfig,
    operation: ReadOperation,
}

pub(crate) fn hostname(host: &str) -> bool {
    host.len() <= 253
        && host.contains('.')
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
        && host.parse::<std::net::IpAddr>().is_err()
}

fn revision(rev: &str) -> bool {
    rev.len() == 40 && rev.bytes().all(|b| b.is_ascii_hexdigit())
}

fn segments(uri: &Uri) -> Option<Vec<&str>> {
    let segments: Vec<_> = uri.path().strip_prefix('/')?.split('/').collect();
    segments
        .iter()
        .all(|s| clean_segment(s))
        .then_some(segments)
}

// References are selectors, never paths outside the approved endpoint.
fn reference(value: &str) -> bool {
    !value.is_empty()
        && value.split('/').all(|s| {
            !s.is_empty()
                && s != "."
                && s != ".."
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.~+".contains(&b))
        })
}

fn archive(kind: UpstreamConfig, repo: String, rev: &str) -> Option<ReadOperation> {
    reference(rev).then(|| ReadOperation::Archive {
        kind,
        repo,
        rev: rev.into(),
    })
}

impl Policy {
    pub fn new(upstreams: Upstreams) -> Result<Self, String> {
        if upstreams.is_empty() || upstreams.len() > 128 {
            return Err("configure 1..=128 upstream hosts".into());
        }
        let mut checked = HashMap::new();
        for (host, kind) in upstreams {
            if !hostname(&host) {
                return Err("upstream keys must be lowercase DNS hostnames, without ports".into());
            }
            let git = GitPolicy::new(&format!("https://{host}/"))?;
            checked.insert(host, Upstream { kind, git });
        }
        Ok(Self { upstreams: checked })
    }

    pub fn hosts(&self) -> impl Iterator<Item = &str> {
        self.upstreams.keys().map(String::as_str)
    }

    pub fn contains(&self, host: &str) -> bool {
        self.upstreams.contains_key(host)
    }

    pub fn authorize(
        &self,
        host: &str,
        method: &Method,
        uri: &Uri,
        headers: &HeaderMap,
        body_empty: bool,
    ) -> Result<AuthorizedRequest, StatusCode> {
        let upstream = self.upstreams.get(host).ok_or(StatusCode::FORBIDDEN)?;
        if uri.scheme().is_some() || uri.authority().is_some() || uri.to_string().len() > 4096 {
            return Err(StatusCode::BAD_REQUEST);
        }
        if upstream.kind.allows_git()
            && let Ok(read) = upstream.git.authorize(method, uri, headers, body_empty)
        {
            return Ok(AuthorizedRequest::Git(read));
        }
        if method != Method::GET && !(method == Method::HEAD && upstream.kind.allows_head()) {
            return Err(StatusCode::FORBIDDEN);
        }
        if !body_empty
            || headers.contains_key("transfer-encoding")
            || headers.contains_key("content-encoding")
            || headers.contains_key("expect")
            || headers.contains_key("upgrade")
            || headers.get_all("content-length").iter().any(|v| v != "0")
            || headers.get_all("connection").iter().any(|v| {
                !v.to_str().is_ok_and(|s| {
                    s.split(',').all(|t| {
                        matches!(
                            t.trim().to_ascii_lowercase().as_str(),
                            "close" | "keep-alive"
                        )
                    })
                })
            })
        {
            return Err(StatusCode::BAD_REQUEST);
        }
        let operation = upstream.kind.read(uri).ok_or(StatusCode::FORBIDDEN)?;
        let mut forwarded = HeaderMap::new();
        for name in ["if-none-match", "if-modified-since", "range", "if-range"] {
            for value in headers.get_all(name) {
                forwarded.append(name, value.clone());
            }
        }
        if upstream.kind == (UpstreamConfig::NixCache {}) {
            for name in ["if-match", "if-unmodified-since"] {
                for value in headers.get_all(name) {
                    forwarded.append(name, value.clone());
                }
            }
        }
        forwarded.insert("accept-encoding", "identity".parse().unwrap());
        if let ReadOperation::Lookup { accept } = &operation {
            forwarded.insert("accept", accept.parse().unwrap());
        }
        let url =
            Url::parse(&format!("https://{host}{uri}")).map_err(|_| StatusCode::BAD_REQUEST)?;
        Ok(AuthorizedRequest::Http(AuthorizedHttpRead {
            method: method.clone(),
            url,
            headers: forwarded,
            kind: upstream.kind,
            operation,
        }))
    }
}

impl AuthorizedHttpRead {
    pub fn method(&self) -> &Method {
        &self.method
    }

    pub fn url(&self) -> &Url {
        &self.url
    }
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    pub fn redirect(self, policy: &Policy, location: &str) -> Result<Self, ()> {
        let url = self.url.join(location).map_err(|_| ())?;
        if url.scheme() != "https"
            || url.port_or_known_default() != Some(443)
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(());
        }
        let host = url.host_str().ok_or(())?;
        let route = match url.query() {
            Some(query) => format!("{}?{query}", url.path()),
            None => url.path().into(),
        };
        let uri = route.parse().map_err(|_| ())?;
        let AuthorizedRequest::Http(next) = policy
            .authorize(host, &self.method, &uri, &self.headers, true)
            .map_err(|_| ())?
        else {
            return Err(());
        };
        if next.operation != self.operation
            || !self
                .kind
                .redirects_to(self.url.host_str().ok_or(())?, next.kind, host)
            || (matches!(self.operation, ReadOperation::Lookup { .. }) && next.url != self.url)
        {
            return Err(());
        }
        Ok(next)
    }
}
