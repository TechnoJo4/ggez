use crate::policy::AuthorizedRead;
use reqwest::{Url, redirect};
use std::{io, time::Duration};

pub struct Client {
    http: reqwest::Client,
    origin: Url,
}

impl Client {
    pub fn new(origin: &str, deadline: Duration) -> Result<Self, String> {
        let url = Url::parse(origin).map_err(|_| "invalid upstream origin")?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err("upstream origin must be an HTTPS origin without credentials, path, query, or fragment".into());
        }
        Self::with_origin(url, deadline)
    }

    fn with_origin(origin: Url, deadline: Duration) -> Result<Self, String> {
        let http = reqwest::Client::builder()
            .https_only(origin.scheme() == "https")
            .no_proxy()
            .redirect(redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(60))
            .timeout(deadline)
            .pool_max_idle_per_host(4)
            .build()
            .map_err(|_| "could not initialize HTTPS client")?;
        Ok(Self { http, origin })
    }

    #[cfg(test)]
    pub(crate) fn for_test(origin: &str) -> Self {
        Self::with_origin(Url::parse(origin).unwrap(), Duration::from_secs(5)).unwrap()
    }

    pub async fn fetch(&self, read: AuthorizedRead) -> io::Result<reqwest::Response> {
        let (method, path_and_query, headers) = read.into_parts();
        let mut url = self.origin.clone();
        let (path, query) = path_and_query
            .split_once('?')
            .map_or((path_and_query.as_str(), None), |(path, query)| {
                (path, Some(query))
            });
        url.set_path(path);
        url.set_query(query);
        self.http
            .request(method, url)
            .headers(headers)
            .send()
            .await
            .map_err(|_| io::Error::other("upstream request failed"))
    }
}
