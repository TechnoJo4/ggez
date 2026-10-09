use crate::policy::{AuthorizedRequest, Policy};
use bytes::Bytes;
use reqwest::{Client as HttpClient, Response, redirect};
use std::{io, time::Duration};

pub(crate) struct Client {
    git: git_policy::Client,
    forge: HttpClient,
}

impl Client {
    #[cfg(test)]
    pub fn for_test(http: HttpClient) -> Self {
        Self {
            git: git_policy::Client::for_test(http.clone()),
            forge: http,
        }
    }

    pub fn new(deadline: Duration) -> Result<Self, String> {
        let forge = HttpClient::builder()
            .https_only(true)
            .no_proxy()
            .redirect(redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(10))
            .timeout(deadline)
            .pool_max_idle_per_host(4)
            .user_agent("ggez-connect-proxy/0.1")
            .build()
            .map_err(|_| "could not initialize forge HTTPS client")?;
        Ok(Self {
            git: git_policy::Client::new(deadline)?,
            forge,
        })
    }

    pub async fn fetch(
        &self,
        request: AuthorizedRequest,
        body: Bytes,
        policy: &Policy,
    ) -> io::Result<Response> {
        let mut read = match request {
            AuthorizedRequest::Git(read) => return self.git.fetch(read, body).await,
            AuthorizedRequest::Forge(read) => read,
        };
        for hop in 0..=3 {
            let response = self
                .forge
                .get(read.url().clone())
                .headers(read.headers().clone())
                .send()
                .await
                .map_err(|_| io::Error::other("forge upstream request failed"))?;
            if !response.status().is_redirection() {
                return Ok(response);
            }
            if hop == 3 || !matches!(response.status().as_u16(), 301 | 302 | 303 | 307 | 308) {
                return Err(io::Error::other("unsupported forge redirect"));
            }
            let location = response
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| io::Error::other("missing forge redirect location"))?;
            read = read
                .redirect(policy, location)
                .map_err(|_| io::Error::other("forge redirect left approved read routes"))?;
        }
        unreachable!()
    }
}
