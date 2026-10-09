use hyper::{HeaderMap, Method, StatusCode, Uri};
use std::collections::HashSet;

pub struct Policy {
    paths: Vec<String>,
    query_keys: HashSet<String>,
}

// No public fields or Deserialize: the upstream client cannot accept unchecked input.
pub struct AuthorizedRead {
    method: Method,
    path_and_query: String,
    headers: HeaderMap,
}

impl AuthorizedRead {
    pub(crate) fn into_parts(self) -> (Method, String, HeaderMap) {
        (self.method, self.path_and_query, self.headers)
    }
}

fn clean_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && segment
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
}

fn clean_path(path: &str) -> bool {
    path == "/"
        || path
            .strip_prefix('/')
            .is_some_and(|tail| tail.split('/').all(clean_segment))
}

impl Policy {
    pub fn new(paths: Vec<String>, query_keys: Vec<String>) -> Result<Self, String> {
        if paths.is_empty() {
            return Err("at least one --read-path-pattern is required".into());
        }
        for path in &paths {
            if path != "/"
                && !path.strip_prefix('/').is_some_and(|tail| {
                    tail.split('/').all(|segment| {
                        !segment.is_empty()
                            && segment.matches('*').count() <= 1
                            && clean_segment(&segment.replace('*', "x"))
                    })
                })
            {
                return Err(format!("invalid read path pattern: {path}"));
            }
        }
        if !query_keys.iter().all(|key| clean_segment(key)) {
            return Err("invalid read query key".into());
        }
        Ok(Self {
            paths,
            query_keys: query_keys.into_iter().collect(),
        })
    }

    pub fn authorize(
        &self,
        method: &Method,
        uri: &Uri,
        headers: &HeaderMap,
        body_empty: bool,
    ) -> Result<AuthorizedRead, StatusCode> {
        if method != Method::GET && method != Method::HEAD {
            return Err(StatusCode::METHOD_NOT_ALLOWED);
        }
        if uri.scheme().is_some()
            || uri.authority().is_some()
            || !clean_path(uri.path())
            || uri.to_string().len() > 4096
        {
            return Err(StatusCode::BAD_REQUEST);
        }
        if !body_empty
            || headers.contains_key("transfer-encoding")
            || headers.contains_key("upgrade")
            || headers.contains_key("expect")
            || headers
                .get_all("content-length")
                .iter()
                .any(|value| value.as_bytes() != b"0")
        {
            return Err(StatusCode::BAD_REQUEST);
        }
        // Only ordinary HTTP connection directives; never propagate nominated headers.
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
        if !self
            .paths
            .iter()
            .any(|pattern| matches_path(pattern, uri.path()))
        {
            return Err(StatusCode::FORBIDDEN);
        }
        if let Some(query) = uri.query() {
            let mut seen = HashSet::new();
            for pair in query.split('&') {
                let Some((key, value)) = pair.split_once('=') else {
                    return Err(StatusCode::BAD_REQUEST);
                };
                if !self.query_keys.contains(key) || !clean_segment(value) || !seen.insert(key) {
                    return Err(StatusCode::FORBIDDEN);
                }
            }
        }
        let mut forwarded = HeaderMap::new();
        for name in [
            "range",
            "if-range",
            "if-none-match",
            "if-modified-since",
            "if-match",
            "if-unmodified-since",
        ] {
            for value in headers.get_all(name) {
                forwarded.append(name, value.clone());
            }
        }
        Ok(AuthorizedRead {
            method: method.clone(),
            path_and_query: uri.path_and_query().map_or("/", |p| p.as_str()).to_owned(),
            headers: forwarded,
        })
    }
}

// '*' matches inside one path segment, never across '/'. No implicit normalization.
fn matches_path(pattern: &str, path: &str) -> bool {
    let pattern: Vec<_> = pattern.split('/').collect();
    let path: Vec<_> = path.split('/').collect();
    pattern.len() == path.len()
        && pattern.iter().zip(path).all(|(pattern, part)| {
            if let Some((prefix, suffix)) = pattern.split_once('*') {
                part.len() >= prefix.len() + suffix.len()
                    && part.starts_with(prefix)
                    && part.ends_with(suffix)
            } else {
                *pattern == part
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> Policy {
        Policy::new(
            vec![
                "/nix-cache-info".into(),
                "/*.narinfo".into(),
                "/nar/*".into(),
            ],
            vec!["sha256".into()],
        )
        .unwrap()
    }

    #[test]
    fn write_and_ambiguous_requests_are_rejected() {
        let policy = policy();
        for method in [
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::CONNECT,
            Method::OPTIONS,
        ] {
            assert!(
                policy
                    .authorize(
                        &method,
                        &"/nix-cache-info".parse().unwrap(),
                        &HeaderMap::new(),
                        true
                    )
                    .is_err()
            );
        }
        for path in [
            "http://elsewhere/nix-cache-info",
            "//elsewhere/nix-cache-info",
            "/nar/../write",
            "/nar/%2e%2e",
            "/nar/a/b",
            "/nar/a?method=delete",
            "/nar/a?sha256=x&sha256=y",
            "/nar/a?sha256=http%3A%2F%2Fevil",
            "/write",
        ] {
            assert!(
                policy
                    .authorize(
                        &Method::GET,
                        &path.parse().unwrap(),
                        &HeaderMap::new(),
                        true
                    )
                    .is_err(),
                "accepted {path}"
            );
        }
        for (name, value) in [
            ("transfer-encoding", "chunked"),
            ("content-length", "1"),
            ("upgrade", "websocket"),
            ("connection", "range"),
            ("expect", "100-continue"),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(name, value.parse().unwrap());
            assert!(
                policy
                    .authorize(
                        &Method::GET,
                        &"/nix-cache-info".parse().unwrap(),
                        &headers,
                        true
                    )
                    .is_err()
            );
        }
        assert!(
            policy
                .authorize(
                    &Method::GET,
                    &"/nix-cache-info".parse().unwrap(),
                    &HeaderMap::new(),
                    false
                )
                .is_err()
        );
    }

    #[test]
    fn reads_preserve_only_explicit_headers() {
        let mut headers = HeaderMap::new();
        for (name, value) in [
            ("host", "evil"),
            ("authorization", "secret"),
            ("cookie", "secret"),
            ("x-http-method-override", "DELETE"),
            ("range", "bytes=1-3"),
        ] {
            headers.insert(name, value.parse().unwrap());
        }
        let read = policy()
            .authorize(
                &Method::HEAD,
                &"/nar/file.nar.xz?sha256=abc123".parse().unwrap(),
                &headers,
                true,
            )
            .unwrap();
        let (method, path, headers) = read.into_parts();
        assert_eq!(method, Method::HEAD);
        assert_eq!(path, "/nar/file.nar.xz?sha256=abc123");
        assert_eq!(headers.len(), 1);
        assert_eq!(headers["range"], "bytes=1-3");
    }
}
