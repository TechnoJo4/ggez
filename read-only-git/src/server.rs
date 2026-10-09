use bytes::Bytes;
use futures_util::StreamExt;
use git_policy::{Client, Operation, Policy};
use http_body_util::{BodyExt, Full, Limited, StreamBody};
use hyper::{
    Request, Response, StatusCode,
    body::{Body, Frame, Incoming},
};
use std::{convert::Infallible, io, sync::Arc, time::Duration};

use host_runtime::http::ResponseBody;

pub struct Gateway {
    pub(crate) policy: Policy,
    pub(crate) client: Client,
    pub(crate) max_body: usize,
}

fn error(status: StatusCode) -> Response<ResponseBody> {
    let mut response = Response::new(
        Full::new(Bytes::from_static(b"git read unavailable\n"))
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
    // Reject writes from the head alone, before reading a body or contacting the host.
    let authorized = match gateway.policy.authorize(
        request.method(),
        request.uri(),
        request.headers(),
        request.body().is_end_stream(),
    ) {
        Ok(read) => read,
        Err(status) => return Ok(error(status)),
    };
    let operation = authorized.operation();
    let body = if operation == Operation::UploadPack {
        match Limited::new(request.into_body(), gateway.max_body)
            .collect()
            .await
        {
            Ok(body) if body.trailers().is_none() => body.to_bytes(),
            Ok(_) => return Ok(error(StatusCode::BAD_REQUEST)),
            Err(_) => return Ok(error(StatusCode::PAYLOAD_TOO_LARGE)),
        }
    } else {
        Bytes::new()
    };
    let upstream = match gateway.client.fetch(authorized, body).await {
        Ok(response) => response,
        Err(_) => return Ok(error(StatusCode::BAD_GATEWAY)),
    };
    let status = upstream.status();
    if status != StatusCode::OK {
        let mut response = error(if status.is_client_error() || status.is_server_error() {
            status
        } else {
            StatusCode::BAD_GATEWAY
        });
        if let Some(value) = upstream.headers().get("retry-after") {
            response.headers_mut().insert("retry-after", value.clone());
        }
        return Ok(response);
    }
    if !upstream
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .eq_ignore_ascii_case(operation.response_type())
        })
    {
        return Ok(error(StatusCode::BAD_GATEWAY));
    }
    let mut headers = hyper::HeaderMap::new();
    for name in [
        "content-type",
        "content-length",
        "content-encoding",
        "cache-control",
        "expires",
    ] {
        if let Some(value) = upstream.headers().get(name) {
            headers.insert(name, value.clone());
        }
    }
    let frames = upstream.bytes_stream().map(|chunk| {
        chunk
            .map(Frame::data)
            .map_err(|_| io::Error::other("git upstream body failed"))
    });
    let mut response = Response::new(StreamBody::new(frames).boxed_unsync());
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
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "read-only-git request deadline"))?
}
