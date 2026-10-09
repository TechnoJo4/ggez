use bytes::Bytes;
use http_body_util::combinators::UnsyncBoxBody;
use hyper::{Request, Response, body::Incoming, server::conn::http1, service::service_fn};
use hyper_util::rt::{TokioIo, TokioTimer};
use std::{convert::Infallible, future::Future, io, time::Duration};
use tokio::io::{AsyncRead, AsyncWrite};

pub type ResponseBody = UnsyncBoxBody<Bytes, io::Error>;

pub async fn connection<S, F, Fut>(stream: S, handle: F) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    F: Fn(Request<Incoming>) -> Fut,
    Fut: Future<Output = Result<Response<ResponseBody>, Infallible>>,
{
    let mut http = http1::Builder::new();
    http.keep_alive(false)
        .half_close(false)
        .max_headers(32)
        .max_buf_size(16384)
        .timer(TokioTimer::new())
        .header_read_timeout(Duration::from_secs(10));
    http.serve_connection(TokioIo::new(stream), service_fn(handle))
        .await
        .map_err(|_| io::Error::other("HTTP connection failed"))
}
