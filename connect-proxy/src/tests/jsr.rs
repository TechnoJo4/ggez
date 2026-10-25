use super::*;

#[tokio::test]
async fn jsr_reads_preserve_bytes_and_mime_types_and_force_registry_negotiation() {
    let reads = [
        (
            "/@std/path/meta.json",
            "application/json",
            &b"{\n \"versions\": {}\n}\n"[..],
        ),
        (
            "/@std/path/1.0.0_meta.json",
            "application/json",
            &b"{\n \"manifest\": {}\n}\n"[..],
        ),
        (
            "/@std/path/1.0.0/posix/mod.ts",
            "text/typescript",
            &b"export const x: number = 1;\r\n"[..],
        ),
        (
            "/@std/path/1.0.0/lib.wasm",
            "application/wasm",
            &[0, 97, 115, 109, 0, 255][..],
        ),
    ];
    let origin = Origin::with_routes(
        reads
            .iter()
            .map(|(path, mime, bytes)| {
                (
                    ("jsr.io".into(), (*path).into()),
                    ((*mime).into(), bytes.to_vec()),
                )
            })
            .collect(),
    )
    .await;
    let gateway = origin.gateway();
    for (path, mime, bytes) in reads {
        for method in ["GET", "HEAD"] {
            let request = format!(
                "{method} {path} HTTP/1.1\r\nHost: jsr.io\r\nAccept: text/html\r\nSec-Fetch-Dest: document\r\nAuthorization: secret\r\nCookie: secret\r\nX-HTTP-Method-Override: DELETE\r\nIf-None-Match: \"cached\"\r\nIf-Modified-Since: Wed, 21 Oct 2015 07:28:00 GMT\r\nRange: bytes=0-1\r\nIf-Range: \"cached\"\r\n\r\n"
            );
            let reply = origin
                .exchange(gateway.clone(), "jsr.io", "jsr.io", request.as_bytes())
                .await
                .unwrap();
            assert!(
                reply.starts_with(b"HTTP/1.1 200"),
                "{}",
                String::from_utf8_lossy(&reply)
            );
            let split = reply.windows(4).position(|b| b == b"\r\n\r\n").unwrap() + 4;
            let headers = String::from_utf8_lossy(&reply[..split]);
            assert!(headers.contains(&format!("content-type: {mime}\r\n")));
            assert_eq!(
                &reply[split..],
                if method == "HEAD" { &[][..] } else { bytes }
            );
        }
        for target in [
            format!("https://jsr.io{path}"),
            "https://jsr.io/@std/path/2.0.0/mod.ts".into(),
            "https://api.jsr.io/scopes/std".into(),
        ] {
            let AuthorizedRequest::Http(read) = gateway
                .policy
                .authorize(
                    "jsr.io",
                    &Method::GET,
                    &path.parse().unwrap(),
                    &HeaderMap::new(),
                    true,
                )
                .unwrap()
            else {
                panic!()
            };
            assert!(read.redirect(&gateway.policy, &target).is_err());
        }
    }
    let requests = origin.requests.lock().unwrap();
    assert_eq!(requests.len(), reads.len() * 2);
    for (_, _, path, headers, body) in requests.iter() {
        assert_eq!(
            headers["accept"],
            if path.ends_with("meta.json") {
                "application/json"
            } else {
                "*/*"
            }
        );
        assert_eq!(headers["accept-encoding"], "identity");
        assert_eq!(headers["if-none-match"], "\"cached\"");
        assert_eq!(
            headers["if-modified-since"],
            "Wed, 21 Oct 2015 07:28:00 GMT"
        );
        assert_eq!(headers["range"], "bytes=0-1");
        assert_eq!(headers["if-range"], "\"cached\"");
        for name in [
            "sec-fetch-dest",
            "authorization",
            "cookie",
            "x-http-method-override",
        ] {
            assert!(!headers.contains_key(name));
        }
        assert!(body.is_empty());
    }
}
