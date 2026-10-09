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
    ]
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
