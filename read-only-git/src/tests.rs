use crate::server::{Gateway, serve};
use bytes::Bytes;
use git_policy::{Client, Policy, UpstreamConfig, Upstreams};
use http_body_util::{BodyExt, Full};
use hyper::{HeaderMap, Method, Response, StatusCode, server::conn::http1, service::service_fn};
use hyper_util::rt::TokioIo;
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

struct Recorded {
    method: Method,
    uri: hyper::Uri,
    headers: HeaderMap,
    body: Bytes,
}
struct Origin {
    url: String,
    requests: Arc<Mutex<Vec<Recorded>>>,
    task: JoinHandle<()>,
}

impl Origin {
    async fn new(
        handler: impl Fn(&Recorded) -> Response<Full<Bytes>> + Send + Sync + 'static,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let handler = Arc::new(handler);
        let task = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let recorded = recorded.clone();
                let handler = handler.clone();
                tokio::spawn(async move {
                    let service =
                        service_fn(move |request: hyper::Request<hyper::body::Incoming>| {
                            let recorded = recorded.clone();
                            let handler = handler.clone();
                            async move {
                                let (parts, body) = request.into_parts();
                                let record = Recorded {
                                    method: parts.method,
                                    uri: parts.uri,
                                    headers: parts.headers,
                                    body: body.collect().await.unwrap().to_bytes(),
                                };
                                let response = tokio::task::spawn_blocking(move || {
                                    let response = handler(&record);
                                    recorded.lock().unwrap().push(record);
                                    response
                                })
                                .await
                                .unwrap();
                                Ok::<_, std::convert::Infallible>(response)
                            }
                        });
                    let _ = http1::Builder::new()
                        .keep_alive(false)
                        .serve_connection(TokioIo::new(stream), service)
                        .await;
                });
            }
        });
        Self {
            url,
            requests,
            task,
        }
    }

    async fn reads() -> Self {
        Self::new(|request| {
            reply(
                StatusCode::OK,
                if request.method == Method::GET {
                    "application/x-git-upload-pack-advertisement"
                } else {
                    "application/x-git-upload-pack-result"
                },
                b"0000".to_vec(),
            )
        })
        .await
    }

    fn gateway(&self) -> Arc<Gateway> {
        gateway(&[("github", &format!("{}/", self.url))])
    }
}

impl Drop for Origin {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn reply(status: StatusCode, content_type: &str, body: Vec<u8>) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::from(body)));
    *response.status_mut() = status;
    response
        .headers_mut()
        .insert("content-type", content_type.parse().unwrap());
    response
}

fn gateway(upstreams: &[(&str, &str)]) -> Arc<Gateway> {
    Arc::new(Gateway {
        policy: Policy::for_test(upstreams),
        client: Client::new(Duration::from_secs(30)).unwrap(),
        max_body: 8 * 1024 * 1024,
    })
}

async fn exchange(gateway: Arc<Gateway>, request: &[u8]) -> Vec<u8> {
    let (mut client, server) = duplex(16384);
    let task = tokio::spawn(serve(server, gateway, Duration::from_secs(5)));
    client.write_all(request).await.unwrap();
    let mut reply = Vec::new();
    timeout(Duration::from_secs(5), client.read_to_end(&mut reply))
        .await
        .unwrap()
        .unwrap();
    let _ = task.await.unwrap();
    reply
}

#[tokio::test]
async fn forbidden_requests_never_reach_an_upstream() {
    let origin = Origin::reads().await;
    for head in [
        "GET /github/group/repo.git/info/refs?service=git-receive-pack",
        "POST /github/group/repo.git/git-receive-pack",
        "POST /github/group/repo.git/info/refs?service=git-upload-pack",
        "GET /github/group/repo.git/git-upload-pack",
        "GET /github/group/repo.git/info/refs",
        "GET /github/group/repo.git/HEAD",
        "GET /github/group/repo.git/objects/info/packs",
        "PUT /github/group/repo.git/git-upload-pack",
        "DELETE /github/group/repo.git/git-upload-pack",
        "GET /unknown/group/repo.git/info/refs?service=git-upload-pack",
        "GET http://evil/github/group/repo.git/info/refs?service=git-upload-pack",
        "GET //evil/github/group/repo.git/info/refs?service=git-upload-pack",
        "GET /github/group/../repo.git/info/refs?service=git-upload-pack",
        "GET /github/group/%2e%2e/repo.git/info/refs?service=git-upload-pack",
        "GET /github/group%2frepo.git/info/refs?service=git-upload-pack",
        "GET /github/group//repo.git/info/refs?service=git-upload-pack",
        "GET /github/group/repo.git/info/refs?service=git-upload-pack&service=git-receive-pack",
        "GET /github/group/repo.git/info/refs?service=git-upload-pack&write=1",
        "GET /github/group/repo.git/info/refs?service=git%2dupload-pack",
        "POST /github/group/repo.git/git-upload-pack?service=git-receive-pack",
        "GET /github/info/refs?service=git-upload-pack",
    ] {
        let request = format!(
            "{head} HTTP/1.1\r\nHost: guest\r\nContent-Type: application/x-git-upload-pack-request\r\nContent-Length: 0\r\n\r\n"
        );
        let reply = exchange(origin.gateway(), request.as_bytes()).await;
        assert!(
            reply.starts_with(b"HTTP/1.1 4"),
            "accepted {head}: {}",
            String::from_utf8_lossy(&reply)
        );
        assert!(origin.requests.lock().unwrap().is_empty());
    }
    for header in [
        "Connection: upgrade\r\nUpgrade: websocket",
        "Connection: x-http-method-override",
        "Git-Protocol: version=2:evil",
        "Git-Protocol: version=1\r\nGit-Protocol: version=2",
        "Transfer-Encoding: chunked",
        "Content-Encoding: gzip",
        "Content-Length: 1",
    ] {
        let request = format!(
            "GET /github/group/repo.git/info/refs?service=git-upload-pack HTTP/1.1\r\nHost: guest\r\n{header}\r\n\r\nx"
        );
        let reply = exchange(origin.gateway(), request.as_bytes()).await;
        assert!(reply.starts_with(b"HTTP/1.1 4"), "accepted {header}");
        assert!(origin.requests.lock().unwrap().is_empty());
    }
    // The body is deliberately absent: receive-pack is rejected from the head.
    let reply = exchange(origin.gateway(), b"POST /github/group/repo.git/git-receive-pack HTTP/1.1\r\nHost: guest\r\nContent-Length: 1000000000\r\nExpect: 100-continue\r\n\r\n").await;
    assert!(reply.starts_with(b"HTTP/1.1 403"));
    assert!(origin.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn named_upstreams_paths_and_protocol_headers_are_rebuilt() {
    let github = Origin::reads().await;
    let gitlab = Origin::reads().await;
    let gateway = gateway(&[
        ("github", &format!("{}/", github.url)),
        ("gitlab", &format!("{}/git/", gitlab.url)),
    ]);
    for (name, version) in [("github", "version=1"), ("gitlab", "version=2")] {
        let request = format!(
            "GET /{name}/group/subgroup/repo.git/info/refs?service=git-upload-pack HTTP/1.1\r\nHost: attacker\r\nGit-Protocol: {version}\r\nAuthorization: guest-secret\r\nCookie: guest-cookie\r\nX-HTTP-Method-Override: DELETE\r\n\r\n"
        );
        assert!(
            exchange(gateway.clone(), request.as_bytes())
                .await
                .starts_with(b"HTTP/1.1 200")
        );
    }
    for (origin, path, version) in [
        (
            &github,
            "/group/subgroup/repo.git/info/refs?service=git-upload-pack",
            "version=1",
        ),
        (
            &gitlab,
            "/git/group/subgroup/repo.git/info/refs?service=git-upload-pack",
            "version=2",
        ),
    ] {
        let recorded = origin.requests.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].uri.to_string(), path);
        assert_eq!(recorded[0].headers["git-protocol"], version);
        assert_ne!(recorded[0].headers["host"], "attacker");
        for name in ["authorization", "cookie", "x-http-method-override"] {
            assert!(!recorded[0].headers.contains_key(name));
        }
    }
}

#[tokio::test]
async fn chunked_and_gzipped_negotiations_preserve_opaque_bytes() {
    let origin = Origin::reads().await;
    for (encoding, body) in [
        ("identity", b"0000".as_slice()),
        // A stored-block gzip member containing "0000".
        ("gzip", b"\x1f\x8b\x08\x00\x00\x00\x00\x00\x00\x03\x01\x04\x00\xfb\xff\x30\x30\x30\x30\x72\xc4\x9b\x0c\x04\x00\x00\x00".as_slice()),
    ] {
        let mut request = format!("POST /github/group/repo.git/git-upload-pack HTTP/1.1\r\nHost: guest\r\nContent-Type: application/x-git-upload-pack-request\r\nContent-Encoding: {encoding}\r\nGit-Protocol: version=2\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n", body.len()).into_bytes();
        request.extend_from_slice(body);
        request.extend_from_slice(b"\r\n0\r\n\r\n");
        assert!(
            exchange(origin.gateway(), &request)
                .await
                .starts_with(b"HTTP/1.1 200")
        );
        let recorded = origin.requests.lock().unwrap();
        let last = recorded.last().unwrap();
        assert_eq!(last.body, body);
        assert_eq!(last.headers["content-encoding"], encoding);
        assert_eq!(
            last.headers["content-type"],
            "application/x-git-upload-pack-request"
        );
    }
}

#[tokio::test]
async fn pack_response_is_binary_with_backpressure_and_no_cookies() {
    let payload: Vec<u8> = (0..100_000).map(|n| n as u8).collect();
    let data = payload.clone();
    let origin = Origin::new(move |_| {
        let mut response = reply(
            StatusCode::OK,
            "application/x-git-upload-pack-result",
            data.clone(),
        );
        response
            .headers_mut()
            .insert("set-cookie", "upstream-secret".parse().unwrap());
        response
    })
    .await;
    let request = b"POST /github/repo.git/git-upload-pack HTTP/1.1\r\nHost: guest\r\nContent-Type: application/x-git-upload-pack-request\r\nContent-Length: 4\r\n\r\n0000";
    let reply = exchange(origin.gateway(), request).await;
    assert!(reply.starts_with(b"HTTP/1.1 200"));
    let split = reply.windows(4).position(|s| s == b"\r\n\r\n").unwrap() + 4;
    assert_eq!(&reply[split..], &payload);
    assert!(
        !String::from_utf8_lossy(&reply[..split])
            .to_ascii_lowercase()
            .contains("set-cookie")
    );
}

#[tokio::test]
async fn invalid_media_bodies_and_trailers_are_rejected_locally() {
    let origin = Origin::reads().await;
    for headers_and_body in [
        "Content-Type: application/x-git-receive-pack-request\r\nContent-Length: 0\r\n\r\n",
        "Content-Type: application/x-git-upload-pack-request\r\nContent-Encoding: br\r\nContent-Length: 0\r\n\r\n",
        "Content-Type: application/x-git-upload-pack-request\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n0000\r\n0\r\nX-HTTP-Method-Override: DELETE\r\n\r\n",
        "Content-Type: application/x-git-upload-pack-request\r\nContent-Length: 9\r\n\r\n123456789",
    ] {
        let mut gateway = origin.gateway();
        Arc::get_mut(&mut gateway).unwrap().max_body = 8;
        let request = format!(
            "POST /github/repo.git/git-upload-pack HTTP/1.1\r\nHost: guest\r\n{headers_and_body}"
        );
        assert!(
            exchange(gateway, request.as_bytes())
                .await
                .starts_with(b"HTTP/1.1 4")
        );
        assert!(origin.requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn redirects_can_rename_a_repo_but_cannot_escape_or_change_the_service() {
    let origin = Origin::new(|request| {
        if request.uri.path().contains("old.git") {
            let mut response = reply(StatusCode::MOVED_PERMANENTLY, "text/plain", Vec::new());
            response.headers_mut().insert(
                "location",
                "/base/group/new.git/info/refs?service=git-upload-pack"
                    .parse()
                    .unwrap(),
            );
            response
        } else {
            reply(
                StatusCode::OK,
                "application/x-git-upload-pack-advertisement",
                b"0000".to_vec(),
            )
        }
    })
    .await;
    let configured = gateway(&[("github", &format!("{}/base/", origin.url))]);
    let response = exchange(configured, b"GET /github/group/old.git/info/refs?service=git-upload-pack HTTP/1.1\r\nHost: guest\r\n\r\n").await;
    assert!(response.starts_with(b"HTTP/1.1 200"));
    assert_eq!(origin.requests.lock().unwrap().len(), 2);
    for location in [
        "https://evil/repo.git/info/refs?service=git-upload-pack",
        "/outside/repo.git/info/refs?service=git-upload-pack",
        "/base/repo.git/info/refs?service=git-receive-pack",
        "/base/repo.git/git-receive-pack",
        "/base/repo.git/info/refs?service=git-upload-pack&method=delete",
        "https://user:secret@example.com/base/repo.git/info/refs?service=git-upload-pack",
        "/base/repo.git/info/refs?service=git-upload-pack#fragment",
    ] {
        let origin = Origin::new(move |_| {
            let mut response = reply(StatusCode::MOVED_PERMANENTLY, "text/plain", Vec::new());
            response
                .headers_mut()
                .insert("location", location.parse().unwrap());
            response
        })
        .await;
        let gateway = gateway(&[("github", &format!("{}/base/", origin.url))]);
        let response = exchange(gateway, b"GET /github/repo.git/info/refs?service=git-upload-pack HTTP/1.1\r\nHost: guest\r\n\r\n").await;
        assert!(response.starts_with(b"HTTP/1.1 502"), "followed {location}");
        assert_eq!(origin.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn redirect_loops_are_bounded_and_error_statuses_survive() {
    let origin = Origin::new(|_| {
        let mut response = reply(StatusCode::TEMPORARY_REDIRECT, "text/plain", Vec::new());
        response.headers_mut().insert(
            "location",
            "/repo.git/info/refs?service=git-upload-pack"
                .parse()
                .unwrap(),
        );
        response
    })
    .await;
    assert!(exchange(origin.gateway(), b"GET /github/repo.git/info/refs?service=git-upload-pack HTTP/1.1\r\nHost: guest\r\n\r\n").await.starts_with(b"HTTP/1.1 502"));
    assert_eq!(origin.requests.lock().unwrap().len(), 4);
    for status in [StatusCode::NOT_FOUND, StatusCode::TOO_MANY_REQUESTS] {
        let origin = Origin::new(move |_| {
            let mut response = reply(status, "text/plain", b"host details".to_vec());
            response
                .headers_mut()
                .insert("retry-after", "17".parse().unwrap());
            response
        })
        .await;
        let response = exchange(origin.gateway(), b"GET /github/repo.git/info/refs?service=git-upload-pack HTTP/1.1\r\nHost: guest\r\n\r\n").await;
        assert!(response.starts_with(format!("HTTP/1.1 {}", status.as_u16()).as_bytes()));
        assert!(String::from_utf8_lossy(&response).contains("retry-after: 17"));
        assert!(!String::from_utf8_lossy(&response).contains("host details"));
        assert_eq!(origin.requests.lock().unwrap().len(), 1);
    }
}

#[test]
fn registry_is_a_whitelist_of_named_https_bases() {
    assert!(Policy::new(Upstreams::new()).is_err());
    for (name, url) in [
        ("github", "http://github.com/"),
        ("github", "https://github.com"),
        ("github", "https://user:secret@github.com/"),
        ("github", "https://github.com/?x=/"),
        ("github", "https://github.com/#/"),
        ("github", "https://github.com/base/../"),
        ("github", "https://github.com/base/%2f/"),
        ("github", "https://github.com//"),
        ("..", "https://github.com/"),
        ("git/hub", "https://github.com/"),
    ] {
        assert!(
            Policy::new(Upstreams::from([(
                name.into(),
                UpstreamConfig::Url(url.into())
            )]))
            .is_err(),
            "accepted {name} -> {url}"
        );
    }
    let config: Upstreams = serde_json::from_str(
        r#"{"github":"https://github.com/","gitlab":"https://gitlab.com/group/"}"#,
    )
    .unwrap();
    assert!(Policy::new(config).is_ok());
    let config: Upstreams = serde_json::from_str(
        r#"{"tangled":{"url":"https://tangled.org/","redirect_bases":["https://knot1.tangled.sh/"]}}"#,
    ).unwrap();
    assert!(Policy::new(config).is_ok());
    for base in [
        "http://knot.example/",
        "https://user@knot.example/",
        "https://knot.example",
        "https://knot.example/base/../",
        "https://knot.example/%2f/",
    ] {
        let config: Upstreams = serde_json::from_value(serde_json::json!({
            "tangled": {"url": "https://tangled.org/", "redirect_bases": [base]}
        }))
        .unwrap();
        assert!(
            Policy::new(config).is_err(),
            "accepted redirect base {base}"
        );
    }
    assert!(
        serde_json::from_str::<Upstreams>(r#"{"github":{"url":"https://github.com/"}}"#).is_err()
    );
    assert!(
        serde_json::from_str::<Upstreams>(
            r#"{"tangled":{"url":"https://tangled.org/","redirect_bases":[],"anything":true}}"#
        )
        .is_err()
    );
}

#[tokio::test]
async fn knot_redirects_require_an_explicit_base_and_a_read_operation() {
    let knot = Origin::reads().await;
    let stranger = Origin::reads().await;
    for (location, allowed) in [
        (
            format!(
                "{}/git/did:plc:abc123/info/refs?service=git-upload-pack",
                knot.url
            ),
            true,
        ),
        (
            format!(
                "{}/git/did:plc:abc123/info/refs?service=git-upload-pack",
                stranger.url
            ),
            false,
        ),
        (
            format!(
                "{}/outside/repo/info/refs?service=git-upload-pack",
                knot.url
            ),
            false,
        ),
        (
            format!("{}/git/repo/info/refs?service=git-receive-pack", knot.url),
            false,
        ),
        (format!("{}/git/repo/git-receive-pack", knot.url), false),
        (format!("{}/git/repo/git-upload-pack", knot.url), false),
        (
            format!(
                "{}/git/repo/info/refs?service=git-upload-pack&write=1",
                knot.url
            ),
            false,
        ),
        (
            format!(
                "{}/git/repo/info/refs?service=git-upload-pack#fragment",
                knot.url
            ),
            false,
        ),
        (
            format!(
                "{}/git/%2e%2e/repo/info/refs?service=git-upload-pack",
                knot.url
            ),
            false,
        ),
    ] {
        let origin = Origin::new(move |_| {
            let mut response = reply(StatusCode::TEMPORARY_REDIRECT, "text/plain", Vec::new());
            response
                .headers_mut()
                .insert("location", location.parse().unwrap());
            response
        })
        .await;
        let configured = Arc::new(Gateway {
            policy: Policy::for_test(&[("tangled", &format!("{}/", origin.url))])
                .with_test_redirects("tangled", &[&format!("{}/git/", knot.url)]),
            client: Client::new(Duration::from_secs(30)).unwrap(),
            max_body: 8 * 1024 * 1024,
        });
        let before = knot.requests.lock().unwrap().len();
        let response = exchange(configured, b"GET /tangled/@user.example/repo/info/refs?service=git-upload-pack HTTP/1.1\r\nHost: guest\r\n\r\n").await;
        assert!(response.starts_with(if allowed {
            b"HTTP/1.1 200"
        } else {
            b"HTTP/1.1 502"
        }));
        assert_eq!(
            knot.requests.lock().unwrap().len(),
            before + usize::from(allowed)
        );
        assert!(stranger.requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn upload_pack_redirects_preserve_the_method_and_negotiation() {
    for status in [
        StatusCode::MOVED_PERMANENTLY,
        StatusCode::FOUND,
        StatusCode::TEMPORARY_REDIRECT,
        StatusCode::PERMANENT_REDIRECT,
    ] {
        let origin = Origin::new(move |request| {
            if request.uri.path().contains("old.git") {
                let mut response = reply(status, "text/plain", Vec::new());
                response
                    .headers_mut()
                    .insert("location", "/new.git/git-upload-pack".parse().unwrap());
                response
            } else {
                reply(
                    StatusCode::OK,
                    "application/x-git-upload-pack-result",
                    b"0000".to_vec(),
                )
            }
        })
        .await;
        let response = exchange(origin.gateway(), b"POST /github/old.git/git-upload-pack HTTP/1.1\r\nHost: guest\r\nContent-Type: application/x-git-upload-pack-request\r\nContent-Length: 4\r\n\r\n0000").await;
        assert!(response.starts_with(b"HTTP/1.1 200"));
        let requests = origin.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(
            requests
                .iter()
                .all(|r| r.method == Method::POST && r.body == b"0000"[..])
        );
    }
}

#[cfg(unix)]
mod git;
