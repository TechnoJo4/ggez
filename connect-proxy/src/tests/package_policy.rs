use super::*;

fn file_path() -> String {
    format!("/packages/aa/bb/{}/pkg-1.0+local.tar.gz", "c".repeat(60))
}

fn allowed() -> Vec<(&'static str, String)> {
    vec![
        ("pypi.org", "/simple/a/".into()),
        ("pypi.org", "/simple/my-package/".into()),
        ("files.pythonhosted.org", file_path()),
        ("files.pythonhosted.org", file_path().replace('+', "%2B")),
        (
            "files.pythonhosted.org",
            file_path().replace("1.0+local", "1%211.0%2blocal"),
        ),
        (
            "files.pythonhosted.org",
            format!("{}.metadata", file_path()),
        ),
        ("index.crates.io", "/config.json".into()),
        ("index.crates.io", "/1/a".into()),
        ("index.crates.io", "/2/ab".into()),
        ("index.crates.io", "/3/a/abc".into()),
        ("index.crates.io", "/ab/cd/abcd".into()),
        ("index.crates.io", "/my/_c/my_crate".into()),
        ("static.crates.io", "/crates/my_crate/1.0.0/download".into()),
        (
            "static.crates.io",
            "/crates/MyCrate/MyCrate-1.0.0-rc.1+build.crate".into(),
        ),
        ("registry.npmjs.org", "/ggez-fixture".into()),
        ("registry.npmjs.org", "/ggez-fixture/latest".into()),
        ("registry.npmjs.org", "/ggez-fixture/1.0.0".into()),
        ("registry.npmjs.org", "/@fixture/pkg".into()),
        ("registry.npmjs.org", "/@fixture%2fpkg".into()),
        ("registry.npmjs.org", "/@fixture%2Fpkg/next".into()),
        (
            "registry.npmjs.org",
            "/@fixture/pkg/1.0.0-rc.1+build".into(),
        ),
        (
            "registry.npmjs.org",
            "/ggez-fixture/-/ggez-fixture-1.0.0.tgz".into(),
        ),
        (
            "registry.npmjs.org",
            "/@fixture/pkg/-/pkg-1.0.0-rc.1+build.tgz".into(),
        ),
        (
            "registry.npmjs.org",
            "/@fixture%2fpkg/-/pkg-1.0.0.tgz".into(),
        ),
        ("npm.jsr.io", "/@jsr/fixture__pkg".into()),
        ("npm.jsr.io", "/@jsr%2ffixture__pkg".into()),
        ("npm.jsr.io", "/~/11/@jsr/fixture__pkg/1.0.0.tgz".into()),
        (
            "npm.jsr.io",
            "/~/12/@jsr/fixture__pkg/1.0.0-rc.1+build.tgz".into(),
        ),
    ]
}

fn denied() -> Vec<(&'static str, String)> {
    vec![
        ("pypi.org", "/simple/".into()),
        ("pypi.org", "/simple/pkg".into()),
        ("pypi.org", "/simple/Some_Pkg/".into()),
        ("pypi.org", "/simple/../".into()),
        ("pypi.org", "/simple/%2e%2e/".into()),
        ("pypi.org", "/simple/pkg/?delete=true".into()),
        ("pypi.org", "/pypi/pkg/json".into()),
        ("pypi.org", "/manage/account/".into()),
        ("files.pythonhosted.org", file_path().replace("/aa/", "/a/")),
        (
            "files.pythonhosted.org",
            file_path().replace("pkg-1.0+local.tar.gz", "../pkg.whl"),
        ),
        (
            "files.pythonhosted.org",
            file_path().replace("pkg-1.0+local.tar.gz", "%2fadmin"),
        ),
        (
            "files.pythonhosted.org",
            file_path().replace("pkg-1.0+local.tar.gz", "%2e%2e"),
        ),
        ("files.pythonhosted.org", file_path().replace('+', "%252B")),
        (
            "files.pythonhosted.org",
            file_path().replace("pkg-1.0+local.tar.gz", "%252fadmin"),
        ),
        (
            "files.pythonhosted.org",
            format!("{}?action=delete", file_path()),
        ),
        (
            "files.pythonhosted.org",
            "/packages/source/p/pkg/pkg-1.tar.gz".into(),
        ),
        ("index.crates.io", "/config.json?x=1".into()),
        ("index.crates.io", "/aa/bb/abcd".into()),
        ("index.crates.io", "/1/ab".into()),
        ("index.crates.io", "/ab/cd/Abcd".into()),
        ("index.crates.io", "/api/v1/crates".into()),
        (
            "static.crates.io",
            "/crates/my_crate/other-1.0.0.crate".into(),
        ),
        ("static.crates.io", "/crates/my_crate/../download".into()),
        (
            "static.crates.io",
            "/crates/my_crate/%2e%2e/download".into(),
        ),
        (
            "static.crates.io",
            "/crates/my_crate/1.0.0/download?x=1".into(),
        ),
        ("static.crates.io", "/api/v1/crates/new".into()),
        ("registry.npmjs.org", "/".into()),
        ("registry.npmjs.org", "/-/".into()),
        ("registry.npmjs.org", "/-/user/org.couchdb.user:lia".into()),
        (
            "registry.npmjs.org",
            "/-/npm/v1/security/advisories/bulk".into(),
        ),
        (
            "registry.npmjs.org",
            "/-/npm/v1/security/audits/quick".into(),
        ),
        ("registry.npmjs.org", "/-/package/pkg/dist-tags".into()),
        ("registry.npmjs.org", "/-/v1/search?text=pkg".into()),
        ("registry.npmjs.org", "/pkg?write=true".into()),
        ("registry.npmjs.org", "/pkg/".into()),
        ("registry.npmjs.org", "/pkg/..".into()),
        ("registry.npmjs.org", "/pkg/%2e%2e".into()),
        ("registry.npmjs.org", "/pkg/-rev/1-abc".into()),
        ("registry.npmjs.org", "/pkg/-/other-1.0.0.tgz".into()),
        ("registry.npmjs.org", "/pkg/-/pkg-../admin.tgz".into()),
        ("registry.npmjs.org", "/pkg/-/pkg-%31.0.0.tgz".into()),
        ("registry.npmjs.org", "/pkg/-/pkg-1.0.0.tgz?x=1".into()),
        ("registry.npmjs.org", "/pkg%2fadmin".into()),
        ("registry.npmjs.org", "/@fixture%252fpkg".into()),
        ("registry.npmjs.org", "/@fixture%2fpkg%2fadmin".into()),
        ("registry.npmjs.org", "/@fixture%2f..".into()),
        ("registry.npmjs.org", "/@fixture/%2e%2e".into()),
        ("registry.npmjs.org", "/@fixture%5cpkg".into()),
        ("registry.npmjs.org", "/@fixture//pkg".into()),
        ("registry.npmjs.org", "/@fixture/pkg/extra/file".into()),
        ("npm.jsr.io", "/~/11/@jsr/fixture__pkg/../meta.json".into()),
        ("npm.jsr.io", "/~/11/@jsr/fixture__pkg/%2e%2e.tgz".into()),
        ("npm.jsr.io", "/~/11/@jsr/fixture__pkg/1.0.0.tgz?x=1".into()),
        ("npm.jsr.io", "/~/x/@jsr/fixture__pkg/1.0.0.tgz".into()),
        ("npm.jsr.io", "/~/11/@other/fixture__pkg/1.0.0.tgz".into()),
        ("npm.jsr.io", "/~/11/@jsr/pkg/1.0.0.tgz".into()),
        ("npm.jsr.io", "/~/11/@jsr/__pkg/1.0.0.tgz".into()),
        ("npm.jsr.io", "/api/scopes/fixture/packages/pkg".into()),
        ("registry.npmjs.org", format!("/{}", "a".repeat(215))),
        (
            "registry.npmjs.org",
            format!("/@{}/{}", "a".repeat(106), "b".repeat(107)),
        ),
    ]
}

#[tokio::test]
async fn npm_metadata_and_tarballs_strip_credentials_and_cannot_redirect() {
    let reads = [
        ("registry.npmjs.org", "/@fixture%2fpkg", "application/json"),
        (
            "registry.npmjs.org",
            "/@fixture/pkg/-/pkg-1.0.0.tgz",
            "application/octet-stream",
        ),
        ("npm.jsr.io", "/@jsr/fixture__pkg", "application/json"),
        (
            "npm.jsr.io",
            "/~/11/@jsr/fixture__pkg/1.0.0.tgz",
            "application/octet-stream",
        ),
    ];
    let origin = Origin::with_routes(
        reads
            .iter()
            .map(|(host, path, content_type)| {
                (
                    ((*host).into(), (*path).into()),
                    ((*content_type).into(), b"fixture".to_vec()),
                )
            })
            .collect(),
    )
    .await;
    let gateway = origin.gateway();
    for (host, path, _) in reads {
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: {host}\r\nAuthorization: secret\r\nCookie: secret\r\nNpm-Session: secret\r\nNpm-Command: publish\r\nNpm-Auth-Type: web\r\nNpm-Scope: @fixture\r\nPacote-Version: 21.5.0\r\nPacote-Req-Type: packument\r\nPacote-Pkg-Id: registry:@jsr/fixture__pkg\r\nPacote-Integrity: sha512-fixture\r\nX-HTTP-Method-Override: DELETE\r\nAccept: text/html\r\nAccept-Encoding: gzip\r\nIf-None-Match: \"cached\"\r\n\r\n"
        );
        let reply = origin
            .exchange(gateway.clone(), host, host, request.as_bytes())
            .await
            .unwrap();
        assert!(
            reply.starts_with(b"HTTP/1.1 200"),
            "{}",
            String::from_utf8_lossy(&reply)
        );
        assert!(!String::from_utf8_lossy(&reply).contains("host-secret"));
        assert!(
            gateway
                .policy
                .authorize(
                    host,
                    &Method::GET,
                    &path.parse().unwrap(),
                    &HeaderMap::new(),
                    false
                )
                .is_err()
        );
        for location in [
            format!("https://registry.npmjs.org{path}"),
            "https://npm.jsr.io/~/11/@jsr/fixture__pkg/1.0.0.tgz".into(),
            "https://unconfigured.example/pkg/-/pkg-1.0.0.tgz".into(),
        ] {
            let AuthorizedRequest::Http(read) = gateway
                .policy
                .authorize(
                    host,
                    &Method::GET,
                    &path.parse().unwrap(),
                    &HeaderMap::new(),
                    true,
                )
                .unwrap()
            else {
                panic!()
            };
            assert!(read.redirect(&gateway.policy, &location).is_err());
        }
    }
    let requests = origin.requests.lock().unwrap();
    assert_eq!(requests.len(), reads.len());
    for (_, _, path, headers, body) in requests.iter() {
        for name in [
            "authorization",
            "cookie",
            "npm-session",
            "npm-command",
            "npm-auth-type",
            "npm-scope",
            "pacote-version",
            "pacote-req-type",
            "pacote-pkg-id",
            "pacote-integrity",
            "x-http-method-override",
        ] {
            assert!(!headers.contains_key(name));
        }
        assert_eq!(headers["if-none-match"], "\"cached\"");
        assert_eq!(headers["accept-encoding"], "identity");
        if !path.ends_with(".tgz") {
            assert_eq!(headers["accept"], "application/json");
        }
        assert!(body.is_empty());
    }
}

#[test]
fn package_policies_only_authorize_their_registry_reads() {
    let policy = Policy::new(upstreams()).unwrap();
    for (host, path) in allowed() {
        for method in [Method::GET, Method::HEAD] {
            assert!(
                policy
                    .authorize(
                        host,
                        &method,
                        &path.parse().unwrap(),
                        &HeaderMap::new(),
                        true
                    )
                    .is_ok(),
                "rejected {host}{path}"
            );
        }
        for method in [
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::PATCH,
            Method::OPTIONS,
        ] {
            assert!(
                policy
                    .authorize(
                        host,
                        &method,
                        &path.parse().unwrap(),
                        &HeaderMap::new(),
                        true
                    )
                    .is_err()
            );
        }
        let AuthorizedRequest::Http(read) = policy
            .authorize(
                host,
                &Method::GET,
                &path.parse().unwrap(),
                &HeaderMap::new(),
                true,
            )
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(read.url().path(), path);
        if !path.ends_with(".tgz") && matches!(host, "registry.npmjs.org" | "npm.jsr.io") {
            assert_eq!(read.headers()["accept"], "application/json");
        }
        assert!(read.redirect(&policy, &path).is_err()); // Even same-host package redirects are denied.
        assert!(
            policy
                .authorize(
                    host,
                    &Method::GET,
                    &"/org/repo.git/info/refs?service=git-upload-pack"
                        .parse()
                        .unwrap(),
                    &HeaderMap::new(),
                    true
                )
                .is_err()
        );
    }
    for (host, path) in denied() {
        assert!(
            policy
                .authorize(
                    host,
                    &Method::GET,
                    &path.parse().unwrap(),
                    &HeaderMap::new(),
                    true
                )
                .is_err(),
            "accepted {host}{path}"
        );
    }
}

#[tokio::test]
async fn package_denials_do_not_reach_upstream_and_reads_strip_credentials() {
    let path = file_path();
    let payload = vec![0, 255, 1, 254];
    let origin = Origin::with_routes(HashMap::from([(
        ("files.pythonhosted.org".into(), path.clone()),
        ("application/octet-stream".into(), payload.clone()),
    )]))
    .await;
    let gateway = origin.gateway();
    for (host, path) in denied().into_iter().chain(allowed()) {
        let request = format!(
            "POST {path} HTTP/1.1\r\nHost: {host}\r\nContent-Length: 999999999\r\nExpect: 100-continue\r\n\r\n"
        );
        assert!(
            origin
                .exchange(gateway.clone(), host, host, request.as_bytes())
                .await
                .unwrap()
                .starts_with(b"HTTP/1.1 4")
        );
    }
    for (host, path) in denied() {
        let request = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\n\r\n");
        assert!(
            origin
                .exchange(gateway.clone(), host, host, request.as_bytes())
                .await
                .unwrap()
                .starts_with(b"HTTP/1.1 4")
        );
    }
    assert!(origin.requests.lock().unwrap().is_empty());
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: files.pythonhosted.org\r\nAuthorization: secret\r\nCookie: secret\r\nX-HTTP-Method-Override: DELETE\r\n\r\n"
    );
    let reply = origin
        .exchange(
            gateway,
            "files.pythonhosted.org",
            "files.pythonhosted.org",
            request.as_bytes(),
        )
        .await
        .unwrap();
    let split = reply.windows(4).position(|b| b == b"\r\n\r\n").unwrap() + 4;
    assert!(reply.starts_with(b"HTTP/1.1 200"));
    assert_eq!(&reply[split..], &payload);
    let recorded = origin.requests.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    for name in ["authorization", "cookie", "x-http-method-override"] {
        assert!(!recorded[0].3.contains_key(name));
    }
}
