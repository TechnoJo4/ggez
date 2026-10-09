use git_policy::{AuthorizedRead as GitRead, GitPolicy, clean_segment};
use hyper::{HeaderMap, Method, StatusCode, Uri};
use reqwest::Url;
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};

#[derive(Clone, Copy, Deserialize)]
#[serde(tag = "policy", rename_all = "kebab-case", deny_unknown_fields)]
pub enum UpstreamConfig {
    Git {},
    Github {},
    GithubApi {},
    GithubCodeload {},
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
enum ForgeRead {
    Commit,
    Archive {
        owner: String,
        repo: String,
        rev: String,
    },
}

// Neither transport nor client can invent an authorized target.
pub(crate) enum AuthorizedRequest {
    Git(GitRead),
    Forge(AuthorizedForgeRead),
}

pub(crate) struct AuthorizedForgeRead {
    url: Url,
    headers: HeaderMap,
    operation: ForgeRead,
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

fn forge_read(kind: UpstreamConfig, path: &str) -> Option<ForgeRead> {
    let segments: Vec<_> = path.strip_prefix('/')?.split('/').collect();
    if !segments.iter().all(|s| clean_segment(s)) {
        return None;
    }
    match kind {
        UpstreamConfig::GithubApi {}
            if segments.len() >= 5 && segments[0] == "repos" && segments[3] == "commits" =>
        {
            Some(ForgeRead::Commit)
        }
        UpstreamConfig::Github {} if segments.len() == 4 && segments[2] == "archive" => {
            let rev = segments[3].strip_suffix(".tar.gz")?;
            revision(rev).then(|| ForgeRead::Archive {
                owner: segments[0].into(),
                repo: segments[1].into(),
                rev: rev.into(),
            })
        }
        UpstreamConfig::GithubApi {}
            if segments.len() == 5
                && segments[0] == "repos"
                && segments[3] == "tarball"
                && revision(segments[4]) =>
        {
            Some(ForgeRead::Archive {
                owner: segments[1].into(),
                repo: segments[2].into(),
                rev: segments[4].into(),
            })
        }
        UpstreamConfig::GithubCodeload {}
            if segments.len() == 4
                && matches!(segments[2], "tar.gz" | "legacy.tar.gz")
                && revision(segments[3]) =>
        {
            Some(ForgeRead::Archive {
                owner: segments[0].into(),
                repo: segments[1].into(),
                rev: segments[3].into(),
            })
        }
        _ => None,
    }
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
        if matches!(
            upstream.kind,
            UpstreamConfig::Git {} | UpstreamConfig::Github {}
        ) && let Ok(read) = upstream.git.authorize(method, uri, headers, body_empty)
        {
            return Ok(AuthorizedRequest::Git(read));
        }
        if method != Method::GET {
            return Err(StatusCode::FORBIDDEN);
        }
        if !body_empty
            || uri.query().is_some()
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
        let operation = forge_read(upstream.kind, uri.path()).ok_or(StatusCode::FORBIDDEN)?;
        let mut forwarded = HeaderMap::new();
        for name in ["if-none-match", "if-modified-since", "range", "if-range"] {
            for value in headers.get_all(name) {
                forwarded.append(name, value.clone());
            }
        }
        forwarded.insert("accept-encoding", "identity".parse().unwrap());
        if operation == ForgeRead::Commit {
            forwarded.insert("accept", "application/vnd.github+json".parse().unwrap());
        }
        let url = Url::parse(&format!("https://{host}{}", uri.path()))
            .map_err(|_| StatusCode::BAD_REQUEST)?;
        Ok(AuthorizedRequest::Forge(AuthorizedForgeRead {
            url,
            headers: forwarded,
            operation,
        }))
    }
}

impl AuthorizedForgeRead {
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
            || url.query().is_some()
        {
            return Err(());
        }
        let host = url.host_str().ok_or(())?;
        let uri = url.path().parse().map_err(|_| ())?;
        let AuthorizedRequest::Forge(next) = policy
            .authorize(host, &Method::GET, &uri, &self.headers, true)
            .map_err(|_| ())?
        else {
            return Err(());
        };
        // Only an archive of the same repository/revision can cross hosts.
        if next.operation != self.operation
            || (matches!(self.operation, ForgeRead::Commit)
                && host != self.url.host_str().ok_or(())?)
        {
            return Err(());
        }
        Ok(next)
    }
}
