use crate::{
    client::Client,
    policy::{AuthorizedRequest, Policy},
};
use bytes::Bytes;
use futures_util::StreamExt;
use http_body_util::{BodyExt, Full, Limited, StreamBody};
use hyper::{
    Request, Response, StatusCode,
    body::{Body, Frame, Incoming},
};
use std::{collections::HashMap, convert::Infallible, io, sync::Arc, time::Duration};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio_rustls::TlsAcceptor;

pub struct Gateway {
    pub(crate) policy: Policy,
    pub(crate) acceptors: HashMap<String, TlsAcceptor>,
    pub(crate) client: Client,
    pub(crate) max_body: usize,
}

use host_runtime::http::ResponseBody;

fn error(status: StatusCode) -> Response<ResponseBody> {
    let mut response = Response::new(
        Full::new(Bytes::from_static(b"read unavailable\n"))
            .map_err(|never| match never {})
            .boxed_unsync(),
    );
    *response.status_mut() = status;
    response
}

pub(crate) fn connect_host(head: &[u8], policy: &Policy) -> Result<String, ()> {
    let mut headers = [httparse::EMPTY_HEADER; 32];
    let mut request = httparse::Request::new(&mut headers);
    if request.parse(head).map_err(|_| ())? != httparse::Status::Complete(head.len())
        || request.method != Some("CONNECT")
        || request.version != Some(1)
    {
        return Err(());
    }
    let authority = request.path.ok_or(())?;
    let host = authority.strip_suffix(":443").ok_or(())?;
    if !policy.contains(host) {
        return Err(());
    }
    let mut host_count = 0;
    for header in request.headers {
        match header.name.to_ascii_lowercase().as_str() {
            "host" => {
                host_count += 1;
                if header.value != authority.as_bytes() {
                    return Err(());
                }
            }
            "content-length" if header.value == b"0" => {}
            "user-agent" | "accept" | "proxy-connection" => {}
            _ => return Err(()),
        }
    }
    if host_count != 1 {
        return Err(());
    }
    Ok(host.into())
}

async fn handle(
    gateway: Arc<Gateway>,
    host: String,
    request: Request<Incoming>,
) -> Result<Response<ResponseBody>, Infallible> {
    // Bind CONNECT, SNI, and HTTP authority. An absolute URI cannot choose a target.
    let mut hosts = request.headers().get_all("host").iter();
    if !hosts
        .next()
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == host || v.strip_suffix(":443") == Some(host.as_str()))
        || hosts.next().is_some()
    {
        return Ok(error(StatusCode::BAD_REQUEST));
    }
    let authorized = match gateway.policy.authorize(
        &host,
        request.method(),
        request.uri(),
        request.headers(),
        request.body().is_end_stream(),
    ) {
        Ok(read) => read,
        Err(status) => return Ok(error(status)),
    };
    let git_operation = match &authorized {
        AuthorizedRequest::Git(read) => Some(read.operation()),
        _ => None,
    };
    let body = if git_operation == Some(git_policy::Operation::UploadPack) {
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
    let upstream = match gateway
        .client
        .fetch(authorized, body, &gateway.policy)
        .await
    {
        Ok(response) => response,
        Err(_) => return Ok(error(StatusCode::BAD_GATEWAY)),
    };
    let status = upstream.status();
    if !matches!(
        status,
        StatusCode::OK | StatusCode::PARTIAL_CONTENT | StatusCode::NOT_MODIFIED
    ) || git_operation.is_some() && status != StatusCode::OK
    {
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
    if git_operation.is_some_and(|operation| {
        !upstream
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
    }) {
        return Ok(error(StatusCode::BAD_GATEWAY));
    }
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
        if let Some(value) = upstream.headers().get(name) {
            headers.insert(name, value.clone());
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
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    tokio::time::timeout(deadline, async {
        // BufReader retains TLS bytes sent in the same write as the CONNECT head.
        let mut stream = BufReader::new(stream);
        let mut head = Vec::new();
        tokio::time::timeout(Duration::from_secs(10), async {
            while head.len() < 16384 {
                head.push(stream.read_u8().await?);
                if head.ends_with(b"\r\n\r\n") {
                    return Ok::<_, io::Error>(());
                }
            }
            Err(io::Error::other("CONNECT head too large"))
        })
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "CONNECT head deadline"))??;
        let host = match connect_host(&head, &gateway.policy) {
            Ok(host) => host,
            Err(_) => {
                stream
                    .write_all(
                        b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .await?;
                return Ok(());
            }
        };
        stream
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await?;
        stream.flush().await?;
        let stream = tokio::time::timeout(
            Duration::from_secs(10),
            gateway.acceptors[&host].accept(stream),
        )
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "TLS handshake deadline"))??;
        host_runtime::http::connection(stream, move |request| {
            handle(gateway.clone(), host.clone(), request)
        })
        .await
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "proxy session deadline"))?
}
