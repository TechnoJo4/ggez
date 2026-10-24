use crate::policy::{AuthorizedRequest, ResponseKind};
use hyper::{
    HeaderMap,
    header::{HeaderName, HeaderValue},
};
use reqwest::{Url, redirect};
use std::{io, time::Duration};

pub(crate) struct Endpoint {
    url: Url,
    headers: HeaderMap,
    pub guest_headers: Vec<HeaderName>,
}

// These can change transport, authority, authentication, or protocol capabilities.
pub(crate) fn reserved_header(name: &str) -> bool {
    matches!(
        name,
        "host"
            | "authorization"
            | "proxy-authorization"
            | "x-api-key"
            | "cookie"
            | "connection"
            | "proxy-connection"
            | "content-length"
            | "transfer-encoding"
            | "trailer"
            | "te"
            | "upgrade"
            | "expect"
            | "content-type"
            | "content-encoding"
            | "accept"
            | "accept-encoding"
            | "anthropic-beta"
            | "anthropic-version"
    )
}

impl Endpoint {
    pub fn new(
        url: &str,
        headers: HeaderMap,
        guest_headers: Vec<HeaderName>,
    ) -> Result<Self, String> {
        let url = Url::parse(url).map_err(|error| format!("invalid endpoint URL: {error}"))?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(
                "endpoint must be an HTTPS URL without credentials, query, or fragment".into(),
            );
        }
        Ok(Self {
            url,
            headers,
            guest_headers,
        })
    }

    #[cfg(test)]
    pub fn for_test(url: &str, key: &str, guest_headers: Vec<HeaderName>) -> Self {
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            HeaderValue::from_str(&format!("Bearer {key}")).unwrap(),
        );
        Self {
            url: Url::parse(url).unwrap(),
            headers,
            guest_headers,
        }
    }
}

pub(crate) struct Client(reqwest::Client);

impl Client {
    pub fn new(deadline: Duration) -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(10))
            .timeout(deadline)
            .pool_max_idle_per_host(4)
            .user_agent("ggez-inference/0.1")
            .build()
            .map_err(|error| {
                format!(
                    "could not initialize inference HTTPS client: {}",
                    host_runtime::error_chain(&error)
                )
            })?;
        Ok(Self(client))
    }

    pub async fn fetch(
        &self,
        request: AuthorizedRequest,
    ) -> io::Result<(reqwest::Response, ResponseKind)> {
        let (endpoint, body, guest_headers, response_kind) = request.into_parts();
        let mut headers = endpoint.headers.clone();
        // Host-set headers win over guest-supplied values, even if explicitly allowed.
        for (name, value) in &guest_headers {
            if !headers.contains_key(name) {
                headers.append(name, value.clone());
            }
        }
        headers.insert("content-type", HeaderValue::from_static("application/json"));
        headers.insert(
            "accept",
            HeaderValue::from_static(response_kind.content_type()),
        );
        headers.insert("accept-encoding", HeaderValue::from_static("identity"));
        let response = self
            .0
            .post(endpoint.url.clone())
            .headers(headers)
            .body(body)
            .send()
            .await
            // Config validation excludes credentials, query strings and fragments.
            .map_err(io::Error::other)?;
        Ok((response, response_kind))
    }
}
