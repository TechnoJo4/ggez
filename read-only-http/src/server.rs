use crate::{client::Client, policy::Policy};
use bytes::Bytes;
use futures_util::StreamExt;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::{
    Request, Response, StatusCode,
    body::{Body, Frame, Incoming},
};
use std::{convert::Infallible, io, sync::Arc, time::Duration};

use host_runtime::http::ResponseBody;

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
    tokio::time::timeout(
        deadline,
        host_runtime::http::connection(stream, move |request| handle(gateway.clone(), request)),
    )
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "read-only-http request deadline"))?
}
