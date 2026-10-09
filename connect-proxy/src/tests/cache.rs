use super::*;

#[test]
fn cache_policy_restricts_paths_queries_methods_and_framing() {
    let policy = Policy::new(upstreams()).unwrap();
    let authorize = |method: &Method, path: &str, headers: &HeaderMap, empty| {
        policy.authorize(
            "cache.nixos.org",
            method,
            &path.parse().unwrap(),
            headers,
            empty,
        )
    };
    for path in [
        "/nix-cache-info",
        "/test.narinfo",
        "/nar/archive.nar",
        "/nar/hash.nar.xz?sha256=abc123",
    ] {
        for method in [Method::GET, Method::HEAD] {
            assert!(authorize(&method, path, &HeaderMap::new(), true).is_ok());
        }
    }
    for path in [
        "/",
        "/admin",
        "/nar/../write",
        "/nar/%2e%2e",
        "/nar/a/b",
        "/nar/a?method=delete",
        "/nar/a?sha256=x&sha256=y",
        "/nar/a?sha256=http%3A%2F%2Fevil",
        "/nar/a?sha256=",
        "https://evil.com/nix-cache-info",
        "/org/repo.git/info/refs?service=git-upload-pack",
    ] {
        assert!(
            authorize(&Method::GET, path, &HeaderMap::new(), true).is_err(),
            "accepted {path}"
        );
    }
    for method in [
        Method::POST,
        Method::PUT,
        Method::DELETE,
        Method::OPTIONS,
        Method::CONNECT,
    ] {
        assert!(authorize(&method, "/nix-cache-info", &HeaderMap::new(), true).is_err());
    }
    for (name, value) in [
        ("transfer-encoding", "chunked"),
        ("content-length", "1"),
        ("upgrade", "websocket"),
        ("connection", "range"),
        ("expect", "100-continue"),
    ] {
        let mut headers = HeaderMap::new();
        headers.insert(name, value.parse().unwrap());
        assert!(authorize(&Method::GET, "/nix-cache-info", &headers, true).is_err());
    }
    assert!(authorize(&Method::GET, "/nix-cache-info", &HeaderMap::new(), false).is_err());
    assert!(
        policy
            .authorize(
                "github.com",
                &Method::HEAD,
                &format!("/org/repo/archive/{REV}.tar.gz").parse().unwrap(),
                &HeaderMap::new(),
                true
            )
            .is_err()
    );
}

#[tokio::test]
async fn cache_tls_reads_preserve_binary_ranges_head_and_misses() {
    let origin = Origin::new().await;
    let gateway = origin.gateway();
    let request = b"GET /nar/archive.nar?sha256=abc123 HTTP/1.1\r\nHost: cache.nixos.org\r\nRange: bytes=0-99999\r\nIf-Range: cache-etag\r\nAuthorization: secret\r\nCookie: secret\r\nX-HTTP-Method-Override: DELETE\r\n\r\n";
    let reply = origin
        .exchange(
            gateway.clone(),
            "cache.nixos.org",
            "cache.nixos.org",
            request,
        )
        .await
        .unwrap();
    let split = reply.windows(4).position(|s| s == b"\r\n\r\n").unwrap() + 4;
    assert!(reply.starts_with(b"HTTP/1.1 206"));
    assert_eq!(&reply[split..], &Cache::default().nar);
    assert!(!String::from_utf8_lossy(&reply[..split]).contains("set-cookie"));
    {
        let records = origin.requests.lock().unwrap();
        assert_eq!(records[0].1, Method::GET);
        assert_eq!(records[0].2, "/nar/archive.nar?sha256=abc123");
        assert_eq!(records[0].3["range"], "bytes=0-99999");
        assert_eq!(records[0].3["if-range"], "cache-etag");
        for name in ["authorization", "cookie", "x-http-method-override"] {
            assert!(!records[0].3.contains_key(name));
        }
    }
    for (method, route, conditional, status) in [
        ("HEAD", "/nar/archive.nar", "", "200"),
        (
            "GET",
            "/test.narinfo",
            "If-None-Match: \"cache-etag\"\r\n",
            "304",
        ),
        ("GET", "/missing.narinfo", "", "404"),
        ("GET", "/nar/redirect.nar", "", "502"),
    ] {
        let request =
            format!("{method} {route} HTTP/1.1\r\nHost: cache.nixos.org\r\n{conditional}\r\n");
        let reply = origin
            .exchange(
                gateway.clone(),
                "cache.nixos.org",
                "cache.nixos.org",
                request.as_bytes(),
            )
            .await
            .unwrap();
        assert!(
            reply.starts_with(format!("HTTP/1.1 {status}").as_bytes()),
            "{}",
            String::from_utf8_lossy(&reply)
        );
        if status == "200" || status == "304" {
            assert!(reply.ends_with(b"\r\n\r\n"));
        }
        assert!(!String::from_utf8_lossy(&reply).contains("location:"));
    }
    let records = origin.requests.lock().unwrap();
    assert_eq!(records.len(), 5); // Redirects never make a second request.
    assert_eq!(records[1].1, Method::HEAD);
}

#[tokio::test]
async fn cache_tls_denials_never_contact_upstream() {
    let origin = Origin::new().await;
    let gateway = origin.gateway();
    for line in [
        "POST /nar/archive.nar",
        "PUT /test.narinfo",
        "DELETE /nix-cache-info",
        "GET /admin",
        "GET /nar/a?delete=true",
    ] {
        let request = format!(
            "{line} HTTP/1.1\r\nHost: cache.nixos.org\r\nContent-Length: 999999999\r\nExpect: 100-continue\r\n\r\n"
        );
        assert!(
            origin
                .exchange(
                    gateway.clone(),
                    "cache.nixos.org",
                    "cache.nixos.org",
                    request.as_bytes()
                )
                .await
                .unwrap()
                .starts_with(b"HTTP/1.1 4")
        );
    }
    assert!(origin.requests.lock().unwrap().is_empty());
}
