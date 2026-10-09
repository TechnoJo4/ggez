use crate::{client::Client, policy::Policy};
use bytes::Bytes;
use futures_util::StreamExt;
use http_body_util::{BodyExt, Full, StreamBody, combinators::UnsyncBoxBody};
use hyper::{
    Request, Response, StatusCode,
    body::{Body, Frame, Incoming},
    server::conn::http1,
    service::service_fn,
};
use hyper_util::rt::{TokioIo, TokioTimer};
use std::{convert::Infallible, io, sync::Arc, time::Duration};

type ResponseBody = UnsyncBoxBody<Bytes, io::Error>;

pub struct Gateway {
    pub policy: Policy,
    pub client: Client,
}

fn error(status: StatusCode) -> Response<ResponseBody> {
    let mut response = Response::new(
        Full::new(Bytes::from_static(b"request unavailable\n"))
            .map_err(|never| match never {})
            .boxed_unsync(),
    );
    *response.status_mut() = status;
    response
}

async fn handle(
    gateway: Arc<Gateway>,
    request: Request<Incoming>,
) -> Result<Response<ResponseBody>, Infallible> {
    let authorized = match gateway.policy.authorize(
        request.method(),
        request.uri(),
        request.headers(),
        request.body().is_end_stream(),
    ) {
        Ok(read) => read,
        Err(status) => return Ok(error(status)),
    };
    let upstream = match gateway.client.fetch(authorized).await {
        Ok(response) => response,
        Err(_) => return Ok(error(StatusCode::BAD_GATEWAY)),
    };
    if upstream.status().is_redirection() && upstream.status() != StatusCode::NOT_MODIFIED
        || upstream.status() == StatusCode::SWITCHING_PROTOCOLS
    {
        return Ok(error(StatusCode::BAD_GATEWAY));
    }
    let status = upstream.status();
    let mut headers = hyper::HeaderMap::new();
    for name in [
        "content-type",
        "content-length",
        "content-encoding",
        "content-range",
        "accept-ranges",
        "etag",
        "last-modified",
        "cache-control",
        "expires",
    ] {
        for value in upstream.headers().get_all(name) {
            headers.append(name, value.clone());
        }
    }
    let frames = upstream.bytes_stream().map(|chunk| {
        chunk
            .map(Frame::data)
            .map_err(|_| io::Error::other("upstream body failed"))
    });
    let mut response = Response::new(StreamBody::new(frames).boxed_unsync());
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    Ok(response)
}

pub async fn serve<S>(stream: S, gateway: Arc<Gateway>, deadline: Duration) -> io::Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let mut http = http1::Builder::new();
    http.keep_alive(false)
        .half_close(false)
        .max_headers(32)
        .max_buf_size(16384)
        .timer(TokioTimer::new())
        .header_read_timeout(Duration::from_secs(10));
    tokio::time::timeout(
        deadline,
        http.serve_connection(
            TokioIo::new(stream),
            service_fn(move |request| handle(gateway.clone(), request)),
        ),
    )
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "request deadline"))?
    .map_err(|_| io::Error::other("HTTP connection failed"))
}
