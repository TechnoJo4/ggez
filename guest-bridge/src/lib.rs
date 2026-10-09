#![forbid(unsafe_code)]

pub mod config;

use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf},
    sync::watch,
    time::{Instant, sleep_until},
};

// Read only through LF. A buffered line reader could swallow the first RPC bytes.
pub async fn handshake<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    guest_port: u32,
) -> io::Result<u32> {
    stream
        .write_all(format!("CONNECT {guest_port}\n").as_bytes())
        .await?;
    let mut line = Vec::with_capacity(14);
    loop {
        if line.len() == 14 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "oversized firecracker acknowledgement",
            ));
        }
        let byte = stream.read_u8().await?;
        if byte == b'\n' {
            break;
        }
        line.push(byte);
    }
    let digits = line
        .strip_prefix(b"OK ")
        .filter(|s| !s.is_empty() && s.iter().all(u8::is_ascii_digit))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid firecracker acknowledgement",
            )
        })?;
    std::str::from_utf8(digits)
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .filter(|p| *p > 0 && *p < u32::MAX)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid assigned host port"))
}

struct Activity<S> {
    stream: S,
    last_io: watch::Sender<Instant>,
}

impl<S: AsyncRead + Unpin> AsyncRead for Activity<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let result = Pin::new(&mut self.stream).poll_read(cx, buf);
        if matches!(result, Poll::Ready(Ok(()))) && buf.filled().len() > before {
            self.last_io.send_replace(Instant::now());
        }
        result
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Activity<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let result = Pin::new(&mut self.stream).poll_write(cx, buf);
        if matches!(result, Poll::Ready(Ok(n)) if n > 0) {
            self.last_io.send_replace(Instant::now());
        }
        result
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

pub async fn relay<A, B>(a: A, b: B, idle_timeout: Duration) -> io::Result<(u64, u64)>
where
    A: AsyncRead + AsyncWrite + Unpin,
    B: AsyncRead + AsyncWrite + Unpin,
{
    let (last_io, observed) = watch::channel(Instant::now());
    let mut a = Activity {
        stream: a,
        last_io: last_io.clone(),
    };
    let mut b = Activity { stream: b, last_io };
    let idle = async {
        loop {
            let deadline = *observed.borrow() + idle_timeout;
            sleep_until(deadline).await;
            if Instant::now() >= *observed.borrow() + idle_timeout {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "session idle timeout",
                ));
            }
        }
    };
    tokio::select! {
        result = tokio::io::copy_bidirectional(&mut a, &mut b) => result,
        result = idle => result,
    }
}

#[cfg(unix)]
pub async fn connect(config: &config::Config) -> io::Result<tokio::net::UnixStream> {
    tokio::time::timeout(config.handshake_timeout, async {
        let mut stream = tokio::net::UnixStream::connect(&config.firecracker_socket).await?;
        handshake(&mut stream, config.guest_port).await?;
        Ok(stream)
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "firecracker handshake timeout"))?
}
