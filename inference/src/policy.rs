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

#[derive(Clone, Copy)]
pub(crate) enum ResponseKind {
    Json,
    EventStream,
}

impl ResponseKind {
    pub fn content_type(self) -> &'static str {
        match self {
            Self::Json => "application/json",
            Self::EventStream => "text/event-stream",
        }
    }
}

pub(crate) struct Route {
    pub endpoint: Arc<Endpoint>,
    pub models: Option<Arc<HashSet<String>>>,
    pub protocol: Protocol,
}

pub(crate) struct Policy {
    routes: HashMap<String, Route>,
}

#[derive(Debug)]
pub(crate) struct Rejection {
    pub status: StatusCode,
    pub reason: String,
}

impl Rejection {
    fn new(status: StatusCode, reason: impl std::fmt::Display) -> Self {
        Self {
            status,
            reason: reason.to_string(),
        }
    }
}

// Constructible only after route, model, and capability validation.
pub(crate) struct AuthorizedRequest {
    endpoint: Arc<Endpoint>,
    body: Bytes,
    headers: HeaderMap,
    response_kind: ResponseKind,
}

impl AuthorizedRequest {
    pub fn into_parts(self) -> (Arc<Endpoint>, Bytes, HeaderMap, ResponseKind) {
        (self.endpoint, self.body, self.headers, self.response_kind)
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
    ) -> Result<AuthorizedRequest, Rejection> {
        if method != Method::POST {
            return Err(Rejection::new(
                StatusCode::METHOD_NOT_ALLOWED,
                "expected POST",
            ));
        }
        if uri.scheme().is_some() || uri.authority().is_some() {
            return Err(Rejection::new(
                StatusCode::BAD_REQUEST,
                "absolute request URIs are not allowed",
            ));
        }
        if headers.contains_key("upgrade") {
            return Err(Rejection::new(
                StatusCode::BAD_REQUEST,
                "protocol upgrades are not allowed",
            ));
        }
        if headers.contains_key("content-encoding") {
            return Err(Rejection::new(
                StatusCode::BAD_REQUEST,
                "request content-encoding is not supported",
            ));
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
                return Err(Rejection::new(
                    StatusCode::BAD_REQUEST,
                    "invalid connection header; only close/keep-alive are allowed",
                ));
            }
        }
        if !crate::content_type(headers, "application/json") {
            return Err(Rejection::new(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "expected content-type application/json",
            ));
        }
        let route = self.routes.get(uri.path()).ok_or_else(|| {
            Rejection::new(StatusCode::NOT_FOUND, "no configured inference route")
        })?;
        // The Anthropic SDK adds this marker; upstream URLs remain host-controlled.
        if uri.query().is_some_and(|query| {
            query != "beta=true" || !matches!(route.protocol, Protocol::Messages)
        }) {
            return Err(Rejection::new(
                StatusCode::BAD_REQUEST,
                "only beta=true is allowed as a query on messages routes",
            ));
        }
        let mut body: serde_json::Value = serde_json::from_slice(body).map_err(|error| {
            Rejection::new(
                StatusCode::BAD_REQUEST,
                format_args!("JSON parse failed ({:?}): {error}", error.classify()),
            )
        })?;
        if !body.is_object() {
            return Err(Rejection::new(
                StatusCode::BAD_REQUEST,
                "schema validation failed: body path=\"\": expected an object",
            ));
        }
        let model = body
            .get("model")
            .and_then(serde_json::Value::as_str)
            .filter(|model| !model.is_empty() && model.len() <= 256)
            .ok_or_else(|| Rejection::new(StatusCode::BAD_REQUEST, "schema validation failed: body path=\"/model\": expected a nonempty string up to 256 bytes"))?;
        if route
            .models
            .as_ref()
            .is_some_and(|models| !models.contains(model))
        {
            return Err(Rejection::new(
                StatusCode::FORBIDDEN,
                "body path=\"/model\": model is not in the upstream whitelist",
            ));
        }
        match route.protocol {
            Protocol::Completions => protocols::completions(&body),
            Protocol::Messages => protocols::messages(&body),
        }
        .map_err(|error| {
            Rejection::new(
                StatusCode::BAD_REQUEST,
                format_args!("schema validation failed: {error}"),
            )
        })?;
        let response_kind = if body
            .get("stream")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            ResponseKind::EventStream
        } else {
            ResponseKind::Json
        };
        body["stream"] =
            serde_json::Value::Bool(matches!(response_kind, ResponseKind::EventStream));
        let mut forwarded = HeaderMap::new();
        for name in &route.endpoint.guest_headers {
            if headers.get_all(name).iter().count() > 1 {
                return Err(Rejection::new(
                    StatusCode::BAD_REQUEST,
                    format_args!("duplicate guest header {name:?}"),
                ));
            }
            if let Some(value) = headers.get(name) {
                forwarded.insert(name.clone(), value.clone());
            }
        }
        // Re-encode what we validated; never forward duplicate-key JSON verbatim.
        let body = serde_json::to_vec(&body)
            .map_err(|error| {
                Rejection::new(
                    StatusCode::BAD_REQUEST,
                    format_args!("could not encode validated request JSON: {error}"),
                )
            })?
            .into();
        Ok(AuthorizedRequest {
            endpoint: route.endpoint.clone(),
            body,
            headers: forwarded,
            response_kind,
        })
    }
}
