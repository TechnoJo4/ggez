use crate::{client::Endpoint, protocols};
use bytes::Bytes;
use hyper::{HeaderMap, Method, StatusCode, Uri};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

#[derive(Clone, Copy)]
pub(crate) enum Protocol {
    Completions,
    Messages,
}

pub(crate) struct Route {
    pub endpoint: Arc<Endpoint>,
    pub models: Arc<HashSet<String>>,
    pub protocol: Protocol,
}

pub(crate) struct Policy {
    routes: HashMap<String, Route>,
}

// Constructible only after route, model, and capability validation.
pub(crate) struct AuthorizedRequest {
    endpoint: Arc<Endpoint>,
    body: Bytes,
    headers: HeaderMap,
}

impl AuthorizedRequest {
    pub fn into_parts(self) -> (Arc<Endpoint>, Bytes, HeaderMap) {
        (self.endpoint, self.body, self.headers)
    }
}

impl Policy {
    pub fn new(routes: HashMap<String, Route>) -> Self {
        Self { routes }
    }

    pub fn authorize(
        &self,
        method: &Method,
        uri: &Uri,
        headers: &HeaderMap,
        body: &[u8],
    ) -> Result<AuthorizedRequest, StatusCode> {
        if method != Method::POST {
            return Err(StatusCode::METHOD_NOT_ALLOWED);
        }
        if uri.scheme().is_some()
            || uri.authority().is_some()
            || uri.query().is_some()
            || headers.contains_key("upgrade")
            || headers.contains_key("content-encoding")
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
        if !crate::json_content_type(headers) {
            return Err(StatusCode::UNSUPPORTED_MEDIA_TYPE);
        }
        let route = self.routes.get(uri.path()).ok_or(StatusCode::NOT_FOUND)?;
        let mut body: serde_json::Value =
            serde_json::from_slice(body).map_err(|_| StatusCode::BAD_REQUEST)?;
        let model = body
            .get("model")
            .and_then(serde_json::Value::as_str)
            .ok_or(StatusCode::BAD_REQUEST)?;
        if !route.models.contains(model) {
            return Err(StatusCode::FORBIDDEN);
        }
        match route.protocol {
            Protocol::Completions => protocols::completions(&body),
            Protocol::Messages => protocols::messages(&body),
        }
        .map_err(|_| StatusCode::BAD_REQUEST)?;
        body["stream"] = serde_json::Value::Bool(false);
        let mut forwarded = HeaderMap::new();
        for name in &route.endpoint.guest_headers {
            if headers.get_all(name).iter().count() > 1 {
                return Err(StatusCode::BAD_REQUEST);
            }
            if let Some(value) = headers.get(name) {
                forwarded.insert(name.clone(), value.clone());
            }
        }
        // Re-encode what we validated; never forward duplicate-key JSON verbatim.
        let body = serde_json::to_vec(&body)
            .map_err(|_| StatusCode::BAD_REQUEST)?
            .into();
        Ok(AuthorizedRequest {
            endpoint: route.endpoint.clone(),
            body,
            headers: forwarded,
        })
    }
}
