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

    pub fn new(deadline: Duration) -> Result<Self, String> {
        let http = HttpClient::builder()
            .no_proxy()
            .redirect(redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(10))
            .timeout(deadline)
            .pool_max_idle_per_host(4)
            .user_agent("ggez-read-only-git/0.1")
            .build()
            .map_err(|_| "could not initialize git HTTPS client")?;
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
                .map_err(|_| io::Error::other("git upstream request failed"))?;
            if !response.status().is_redirection() {
                return Ok(response);
            }
            if hop == 3
                || !matches!(response.status().as_u16(), 301 | 302 | 303 | 307 | 308)
                || read.operation() == Operation::UploadPack && response.status().as_u16() == 303
            {
                return Err(io::Error::other("unsupported git redirect"));
            }
            let location = response
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| io::Error::other("missing git redirect location"))?;
            read.redirect(location)
                .map_err(|_| io::Error::other("git redirect left approved read routes"))?;
        }
        unreachable!()
    }
}
