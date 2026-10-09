use crate::{
    client::Client,
    policy::Policy,
    server::{Gateway, serve},
};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, duplex},
    net::TcpListener,
    task::JoinHandle,
    time::timeout,
};

struct Origin {
    url: String,
    requests: Arc<Mutex<Vec<String>>>,
    task: JoinHandle<()>,
}

impl Origin {
    async fn new(response: Vec<u8>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let task = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    request.push(stream.read_u8().await.unwrap());
                    assert!(request.len() < 16384);
                }
                recorded
                    .lock()
                    .unwrap()
                    .push(String::from_utf8(request).unwrap());
                if stream.write_all(&response).await.is_ok() {
                    let _ = stream.shutdown().await;
                }
            }
        });
        Self {
            url,
            requests,
            task,
        }
    }

    fn gateway(&self) -> Arc<Gateway> {
        Arc::new(Gateway {
            policy: Policy::new(
                vec![
                    "/nix-cache-info".into(),
                    "/nar/*".into(),
                    "/*.narinfo".into(),
                ],
                vec!["sha256".into()],
            )
            .unwrap(),
            client: Client::for_test(&self.url),
        })
    }
}

impl Drop for Origin {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn exchange(gateway: Arc<Gateway>, request: &[u8]) -> Vec<u8> {
    let (mut client, server) = duplex(1024);
    let task = tokio::spawn(serve(server, gateway, Duration::from_secs(5)));
    client.write_all(request).await.unwrap();
    let mut reply = Vec::new();
    timeout(Duration::from_secs(5), client.read_to_end(&mut reply))
        .await
        .unwrap()
        .unwrap();
    // Parser errors can end the connection after writing their own 4xx response.
    let _ = task.await.unwrap();
    reply
}

#[tokio::test]
async fn forbidden_wire_requests_make_zero_upstream_calls() {
    let origin =
        Origin::new(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec())
            .await;
    for request in [
        "POST /nix-cache-info HTTP/1.1\r\nHost: guest\r\nContent-Length: 0\r\n\r\n",
        "GET /admin HTTP/1.1\r\nHost: guest\r\n\r\n",
        "GET http://evil/nix-cache-info HTTP/1.1\r\nHost: guest\r\n\r\n",
        "GET /nar/%2fadmin HTTP/1.1\r\nHost: guest\r\n\r\n",
        "GET /nar/x?method=delete HTTP/1.1\r\nHost: guest\r\n\r\n",
        "GET /nix-cache-info HTTP/1.1\r\nHost: guest\r\nContent-Length: 1\r\n\r\nx",
        "GET /nix-cache-info HTTP/1.1\r\nHost: guest\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n",
        "GET /nix-cache-info HTTP/1.1\r\nHost: guest\r\nConnection: upgrade\r\nUpgrade: websocket\r\n\r\n",
        "GET /nix-cache-info HTTP/1.1\r\nHost: guest\r\nContent-Length: 0\r\nContent-Length: 1\r\n\r\nx",
    ] {
        let reply = exchange(origin.gateway(), request.as_bytes()).await;
        assert!(
            reply.starts_with(b"HTTP/1.1 4"),
            "unexpected reply: {}",
            String::from_utf8_lossy(&reply)
        );
        assert!(
            origin.requests.lock().unwrap().is_empty(),
            "forbidden request reached upstream"
        );
    }
}

#[tokio::test]
async fn range_read_rebuilds_headers_and_streams_binary_body() {
    let payload: Vec<u8> = (0..100_000).map(|n| n as u8).collect();
    let mut response = format!("HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Type: application/x-nix-nar\r\nContent-Range: bytes 0-99999/100000\r\nSet-Cookie: upstream-secret\r\nConnection: close\r\n\r\n", payload.len()).into_bytes();
    response.extend(&payload);
    let origin = Origin::new(response).await;
    let reply = exchange(origin.gateway(), b"GET /nar/hash.nar.xz?sha256=abc123 HTTP/1.1\r\nHost: attacker\r\nAuthorization: guest-secret\r\nCookie: guest-cookie\r\nX-HTTP-Method-Override: DELETE\r\nRange: bytes=0-99999\r\n\r\n").await;
    let split = reply.windows(4).position(|s| s == b"\r\n\r\n").unwrap() + 4;
    assert!(reply.starts_with(b"HTTP/1.1 206"));
    assert_eq!(&reply[split..], &payload);
    assert!(
        !String::from_utf8_lossy(&reply[..split])
            .to_lowercase()
            .contains("set-cookie")
    );
    let recorded = origin.requests.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    let request = recorded[0].to_ascii_lowercase();
    assert!(request.starts_with("get /nar/hash.nar.xz?sha256=abc123 http/1.1"));
    assert!(request.contains("range: bytes=0-99999"));
    for forbidden in [
        "attacker",
        "guest-secret",
        "guest-cookie",
        "method-override",
    ] {
        assert!(!request.contains(forbidden));
    }
}

#[tokio::test]
async fn redirects_are_not_followed_or_exposed() {
    let origin = Origin::new(b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/escape\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()).await;
    let reply = exchange(
        origin.gateway(),
        b"GET /nix-cache-info HTTP/1.1\r\nHost: guest\r\n\r\n",
    )
    .await;
    assert!(reply.starts_with(b"HTTP/1.1 502"));
    assert!(!String::from_utf8_lossy(&reply).contains("escape"));
    assert_eq!(origin.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn head_and_conditional_not_modified_have_no_body() {
    for (method, status) in [("HEAD", "200 OK"), ("GET", "304 Not Modified")] {
        let origin = Origin::new(
            format!(
                "HTTP/1.1 {status}\r\nContent-Length: 0\r\nETag: xyz\r\nConnection: close\r\n\r\n"
            )
            .into_bytes(),
        )
        .await;
        let reply = exchange(
            origin.gateway(),
            format!(
                "{method} /nix-cache-info HTTP/1.1\r\nHost: guest\r\nIf-None-Match: xyz\r\n\r\n"
            )
            .as_bytes(),
        )
        .await;
        assert!(reply.starts_with(format!("HTTP/1.1 {status}").as_bytes()));
        assert!(reply.ends_with(b"\r\n\r\n"));
        assert!(
            origin.requests.lock().unwrap()[0]
                .to_lowercase()
                .contains("if-none-match: xyz")
        );
    }
}

#[test]
fn production_origin_cannot_select_http_credentials_or_paths() {
    for origin in [
        "http://localhost",
        "https://user:secret@example.com",
        "https://example.com/path",
        "https://example.com?url=evil",
        "https://example.com#fragment",
    ] {
        assert!(Client::new(origin, Duration::from_secs(1)).is_err());
    }
}
