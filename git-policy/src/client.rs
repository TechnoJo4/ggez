use crate::{AuthorizedRead, Operation};
use bytes::Bytes;
use reqwest::{Client as HttpClient, Response, redirect};
use std::{io, time::Duration};

pub struct Client(HttpClient);

impl Client {
    #[cfg(feature = "test-upstreams")]
    pub fn for_test(http: HttpClient) -> Self {
        Self(http)
    }

    pub fn new(deadline: Duration) -> Result<Self, reqwest::Error> {
        let http = HttpClient::builder()
            .no_proxy()
            .redirect(redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(10))
            .timeout(deadline)
            .pool_max_idle_per_host(4)
            .user_agent("ggez-read-only-git/0.1")
            .build()?;
        Ok(Self(http))
    }

    pub async fn fetch(&self, mut read: AuthorizedRead, body: Bytes) -> io::Result<Response> {
        for hop in 0..=3 {
            let mut request = self
                .0
                .request(read.operation().method(), read.url().clone())
                .headers(read.headers().clone());
            if read.operation() == Operation::UploadPack {
                request = request.body(body.clone());
            }
            let response = request
                .send()
                .await
                .map_err(|error| io::Error::other(error.without_url()))?;
            if !response.status().is_redirection() {
                return Ok(response);
            }
            let status = response.status();
            if hop == 3 {
                return Err(io::Error::other(format!(
                    "git redirect limit exceeded at host={:?} path={:?} (status {status})",
                    read.url().host_str(),
                    read.url().path()
                )));
            }
            if !matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308)
                || read.operation() == Operation::UploadPack && response.status().as_u16() == 303
            {
                return Err(io::Error::other(format!(
                    "unsupported git redirect status {status} for {} at host={:?} path={:?}",
                    read.operation().method(),
                    read.url().host_str(),
                    read.url().path()
                )));
            }
            let location = response
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| io::Error::other(format!("missing or invalid git redirect Location (status {status}, host={:?} path={:?})", read.url().host_str(), read.url().path())))?;
            let target = match read.url().join(location) {
                Ok(url) => format!(
                    "origin={:?} path={:?}",
                    url.origin().ascii_serialization(),
                    url.path()
                ),
                Err(error) => format!("invalid URL: {error}"),
            };
            read.redirect(location).map_err(|_| {
                io::Error::other(format!(
                    "git redirect left approved read routes: {target} (status {status})"
                ))
            })?;
        }
        unreachable!()
    }
}
