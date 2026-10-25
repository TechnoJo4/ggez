use crate::{
    client::Client,
    policy::{Policy, ResponseKind},
};
use bytes::Bytes;
use futures_util::StreamExt;
use http_body_util::{BodyExt, Full, LengthLimitError, Limited, StreamBody};
use hyper::{
    Request, Response, StatusCode,
    body::{Body, Frame, Incoming},
};
use std::{
    convert::Infallible,
    future::Future,
    io,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use host_runtime::http::ResponseBody;

pub struct Gateway {
    pub(crate) policy: Policy,
    pub(crate) client: Client,
    pub(crate) max_body: usize,
    pub(crate) max_response_body: usize,
}

// Buffer one complete JSON document. Whitespace is emitted only before it,
// never interleaved with a provider's partial JSON chunks.
struct JsonBody {
    response: Pin<Box<dyn Future<Output = io::Result<Bytes>> + Send>>,
    heartbeat: tokio::time::Interval,
    finished: bool,
    context: String,
}

impl JsonBody {
    fn new(mut upstream: reqwest::Response, limit: usize, context: String) -> Self {
        let response = Box::pin(async move {
            let mut bytes = Vec::new();
            while let Some(chunk) = upstream
                .chunk()
                .await
                .map_err(|cause| io::Error::other(cause.without_url()))?
            {
                if chunk.len() > limit.saturating_sub(bytes.len()) {
                    return Err(io::Error::other(format!(
                        "inference response exceeds {limit} byte size bound"
                    )));
                }
                bytes.extend_from_slice(&chunk);
            }
            let json: serde_json::Value = serde_json::from_slice(&bytes).map_err(|cause| {
                io::Error::other(format!("invalid inference JSON response: {cause}"))
            })?;
            if !json.is_object() {
                return Err(io::Error::other("inference response must be a JSON object"));
            }
            Ok(bytes.into())
        });
        let mut heartbeat = tokio::time::interval(Duration::from_secs(10));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        Self {
            response,
            heartbeat,
            finished: false,
            context,
        }
    }
}

impl Body for JsonBody {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        if self.finished {
            return Poll::Ready(None);
        }
        if let Poll::Ready(result) = self.response.as_mut().poll(cx) {
            self.finished = true;
            if let Err(cause) = &result {
                eprintln!(
                    "inference: {}: response body failed: {}",
                    self.context,
                    host_runtime::error_chain(cause)
                );
            }
            return Poll::Ready(Some(result.map(Frame::data)));
        }
        match self.heartbeat.poll_tick(cx) {
            Poll::Ready(_) => Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(b"\n"))))),
            Poll::Pending => Poll::Pending,
        }
    }
}

fn event_stream_body(
    upstream: reqwest::Response,
    mut limit: usize,
    context: String,
) -> ResponseBody {
    let frames = upstream.bytes_stream().map(move |chunk| {
        let chunk = chunk.map_err(|cause| {
            let cause = cause.without_url();
            eprintln!("inference: {context}: event stream failed: {}", host_runtime::error_chain(&cause));
            io::Error::other(cause)
        })?;
        if chunk.len() > limit {
            eprintln!("inference: {context}: event stream exceeds size bound ({} byte chunk, {limit} bytes remaining)", chunk.len());
            return Err(io::Error::other("inference response exceeds size bound"));
        }
        limit -= chunk.len();
        Ok(Frame::data(chunk))
    });
    StreamBody::new(frames).boxed_unsync()
}

fn error(status: StatusCode) -> Response<ResponseBody> {
    let body = Full::new(Bytes::from_static(b"{\"type\":\"error\",\"error\":{\"type\":\"api_error\",\"message\":\"inference request unavailable\"}}"))
        .map_err(|never| match never {}).boxed_unsync();
    let mut response = Response::new(body);
    *response.status_mut() = status;
    response.headers_mut().insert(
        "content-type",
        hyper::header::HeaderValue::from_static("application/json"),
    );
    response
}

fn bad_gateway(context: &str, cause: impl std::fmt::Display) -> Response<ResponseBody> {
    eprintln!("inference: 502 {context}: {cause}");
    error(StatusCode::BAD_GATEWAY)
}

async fn handle(
    gateway: Arc<Gateway>,
    request: Request<Incoming>,
) -> Result<Response<ResponseBody>, Infallible> {
    let (parts, body) = request.into_parts();
    let context = format!("{} path={:?}", parts.method, parts.uri.path());
    let body = match Limited::new(body, gateway.max_body).collect().await {
        Ok(body) if body.trailers().is_none() => body.to_bytes(),
        Ok(_) => {
            eprintln!("inference: 400 {context}: request trailers are not allowed");
            return Ok(error(StatusCode::BAD_REQUEST));
        }
        Err(cause) => {
            let status = if cause.is::<LengthLimitError>() {
                eprintln!(
                    "inference: 413 {context}: request body exceeds {} byte size bound: {}",
                    gateway.max_body,
                    host_runtime::error_chain(cause.as_ref())
                );
                StatusCode::PAYLOAD_TOO_LARGE
            } else {
                eprintln!(
                    "inference: 400 {context}: request body read failed: {}",
                    host_runtime::error_chain(cause.as_ref())
                );
                StatusCode::BAD_REQUEST
            };
            return Ok(error(status));
        }
    };
    let authorized =
        match gateway
            .policy
            .authorize(&parts.method, &parts.uri, &parts.headers, &body)
        {
            Ok(request) => request,
            Err(rejection) => {
                eprintln!(
                    "inference: {} {context}: {}",
                    rejection.status, rejection.reason
                );
                return Ok(error(rejection.status));
            }
        };
    let (upstream, response_kind) = match gateway.client.fetch(authorized).await {
        Ok(response) => response,
        Err(cause) => return Ok(bad_gateway(&context, host_runtime::error_chain(&cause))),
    };
    let context = format!(
        "{context}, upstream host={:?} path={:?}, request-id={:?}",
        upstream.url().host_str(),
        upstream.url().path(),
        upstream
            .headers()
            .get("x-request-id")
            .or_else(|| upstream.headers().get("request-id"))
    );
    let status = upstream.status();
    if status.is_redirection() || status == StatusCode::SWITCHING_PROTOCOLS {
        return Ok(bad_gateway(
            &context,
            format_args!("unsupported upstream status {status}"),
        ));
    }
    let mut headers = hyper::HeaderMap::new();
    for name in ["content-type", "retry-after", "x-request-id", "request-id"] {
        if let Some(value) = upstream.headers().get(name) {
            headers.insert(name, value.clone());
        }
    }
    if status.is_client_error() || status.is_server_error() {
        eprintln!("inference: {status} {context}: upstream error");
        // Keep status/backoff semantics, without reflecting credential-bearing provider errors.
        let mut response = error(status);
        headers.remove("content-type");
        response.headers_mut().extend(headers);
        return Ok(response);
    }
    if !crate::content_type(upstream.headers(), response_kind.content_type()) {
        return Ok(bad_gateway(
            &context,
            format_args!(
                "invalid response content-type {:?}; expected {:?}",
                upstream.headers().get("content-type"),
                response_kind.content_type()
            ),
        ));
    }
    if upstream
        .headers()
        .get("content-encoding")
        .is_some_and(|v| v != "identity")
    {
        return Ok(bad_gateway(
            &context,
            format_args!(
                "unsupported response content-encoding {:?}",
                upstream.headers().get("content-encoding")
            ),
        ));
    }
    if upstream
        .content_length()
        .is_some_and(|n| n > gateway.max_response_body as u64)
    {
        return Ok(bad_gateway(
            &context,
            format_args!(
                "response content-length {:?} exceeds {} byte size bound",
                upstream.content_length(),
                gateway.max_response_body
            ),
        ));
    }
    let body = match response_kind {
        ResponseKind::Json => {
            JsonBody::new(upstream, gateway.max_response_body, context).boxed_unsync()
        }
        ResponseKind::EventStream => {
            event_stream_body(upstream, gateway.max_response_body, context)
        }
    };
    let mut response = Response::new(body);
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    Ok(response)
}

pub async fn serve<S>(stream: S, gateway: Arc<Gateway>, deadline: Duration) -> io::Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    tokio::time::timeout(
        deadline,
        host_runtime::http::connection(stream, move |request| handle(gateway.clone(), request)),
    )
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "inference request deadline"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn keepalives_repeat_until_the_complete_document_is_ready() {
        let (send, receive) = tokio::sync::oneshot::channel();
        let mut body = JsonBody {
            response: Box::pin(async { Ok(receive.await.unwrap()) }),
            heartbeat: tokio::time::interval(Duration::from_secs(10)),
            finished: false,
            context: "keepalive test".into(),
        };
        assert_eq!(
            body.frame().await.unwrap().unwrap().into_data().unwrap(),
            b"\n"[..]
        );
        let now = tokio::time::Instant::now();
        assert_eq!(
            body.frame().await.unwrap().unwrap().into_data().unwrap(),
            b"\n"[..]
        );
        assert_eq!(now.elapsed(), Duration::from_secs(10));
        send.send(Bytes::from_static(b"{\"ok\":true}")).unwrap();
        assert_eq!(
            body.frame().await.unwrap().unwrap().into_data().unwrap(),
            b"{\"ok\":true}"[..]
        );
        assert!(body.frame().await.is_none());
    }
}
