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

fn bad_gateway(context: &str, cause: impl std::fmt::Display) -> Response<ResponseBody> {
    eprintln!("read-only-git: 502 {context}: {cause}");
    error(StatusCode::BAD_GATEWAY)
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
    let context = format!(
        "{} path={:?}, upstream host={:?} path={:?}",
        request.method(),
        request.uri().path(),
        authorized.url().host_str(),
        authorized.url().path()
    );
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
        Err(cause) => return Ok(bad_gateway(&context, host_runtime::error_chain(&cause))),
    };
    let status = upstream.status();
    if status != StatusCode::OK {
        let response_status = if status.is_client_error() || status.is_server_error() {
            status
        } else {
            StatusCode::BAD_GATEWAY
        };
        if response_status.is_server_error() {
            eprintln!(
                "read-only-git: {response_status} {context}: upstream returned {status} from host={:?} path={:?}",
                upstream.url().host_str(),
                upstream.url().path()
            );
        }
        let mut response = error(response_status);
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
        return Ok(bad_gateway(
            &context,
            format_args!(
                "invalid Git response content-type {:?}; expected {:?}",
                upstream.headers().get("content-type"),
                operation.response_type()
            ),
        ));
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
    let frames = upstream.bytes_stream().map(move |chunk| {
        chunk.map(Frame::data).map_err(|cause| {
            let cause = cause.without_url();
            eprintln!(
                "read-only-git: {context}: upstream body failed: {}",
                host_runtime::error_chain(&cause)
            );
            io::Error::other(cause)
        })
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
