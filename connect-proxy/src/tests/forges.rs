use super::*;

fn authorize(
    policy: &Policy,
    host: &str,
    method: &Method,
    path: &str,
) -> Result<AuthorizedRequest, StatusCode> {
    policy.authorize(
        host,
        method,
        &path.parse().unwrap(),
        &HeaderMap::new(),
        true,
    )
}

#[test]
fn github_atom_feeds_only_admit_release_and_branch_reads() {
    let policy = Policy::new(upstreams()).unwrap();
    for path in [
        "/org/repo/releases.atom",
        "/org/repo/commits/main.atom",
        "/org/repo/commits/feature/branch.atom",
        "/org/repo/commits/release/v1.2-rc_1.atom",
    ] {
        let read = || match authorize(&policy, "github.com", &Method::GET, path).unwrap() {
            AuthorizedRequest::Http(read) => read,
            _ => panic!(),
        };
        assert_eq!(read().url().as_str(), format!("https://github.com{path}"));
        assert_eq!(read().headers()["accept"], "application/atom+xml");
        assert!(read().redirect(&policy, path).is_ok());
        for location in [
            path.replace("org", "other"),
            "/org/repo/commits/other.atom".into(),
            format!("https://api.github.com{path}"),
            format!("/org/repo/archive/{REV}.tar.gz"),
        ] {
            assert!(
                read().redirect(&policy, &location).is_err(),
                "followed {location}"
            );
        }
        for method in [Method::HEAD, Method::POST, Method::PUT, Method::DELETE] {
            assert!(authorize(&policy, "github.com", &method, path).is_err());
        }
        assert!(authorize(&policy, "api.github.com", &Method::GET, path).is_err());
    }
    for path in [
        "/org/repo/releases",
        "/org/repo/releases.atom/extra",
        "/org/repo/releases.atom?method=delete",
        "/org/repo/commits.atom",
        "/org/repo/commits/main",
        "/org/repo/commits/.atom",
        "/org/repo/commits/..atom",
        "/org/repo/commits/...atom",
        "/org/repo/commits//main.atom",
        "/org/repo/commits/../main.atom",
        "/org/repo/commits/%2e%2e/main.atom",
        "/org/repo/commits/feature%2Fbranch.atom",
        "/org/repo/commits/main.atom/extra",
        "/org/repo/commits/main.atom?path=secret",
        "/org/repo/issues.atom",
    ] {
        assert!(
            authorize(&policy, "github.com", &Method::GET, path).is_err(),
            "accepted {path}"
        );
    }
    let plain = Policy::new(Upstreams::from([(
        "github.com".into(),
        UpstreamConfig::Git {},
    )]))
    .unwrap();
    assert!(
        authorize(
            &plain,
            "github.com",
            &Method::GET,
            "/org/repo/releases.atom"
        )
        .is_err()
    );
}

#[tokio::test]
async fn github_atom_feeds_pass_through_tls_with_conditional_headers() {
    let origin = Origin::new().await;
    for path in [
        "/org/repo/releases.atom",
        "/org/repo/commits/feature/branch.atom",
    ] {
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: github.com\r\nAccept: text/html\r\nIf-None-Match: \"feed-etag\"\r\nIf-Modified-Since: Wed, 21 Oct 2015 07:28:00 GMT\r\nAuthorization: secret\r\nCookie: secret\r\n\r\n"
        );
        let reply = origin
            .exchange(
                origin.gateway(),
                "github.com",
                "github.com",
                request.as_bytes(),
            )
            .await
            .unwrap();
        let reply = String::from_utf8_lossy(&reply);
        assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
        assert!(
            reply.contains("content-type: application/atom+xml"),
            "{reply}"
        );
        assert!(
            reply.contains("<feed xmlns=\"http://www.w3.org/2005/Atom\"/>"),
            "{reply}"
        );
        let records = origin.requests.lock().unwrap();
        let last = records.last().unwrap();
        assert_eq!((&*last.0, &*last.2), ("github.com", path));
        assert_eq!(last.3["accept"], "application/atom+xml");
        assert_eq!(last.3["if-none-match"], "\"feed-etag\"");
        assert_eq!(last.3["if-modified-since"], "Wed, 21 Oct 2015 07:28:00 GMT");
        assert!(!last.3.contains_key("authorization"));
        assert!(!last.3.contains_key("cookie"));
    }
}

#[test]
fn forge_routes_accept_nix_selectors_without_admitting_other_api_capabilities() {
    let policy = Policy::new(upstreams()).unwrap();
    for (host, path) in [
        ("codeberg.org", format!("/org/repo/archive/{REV}.tar.gz")),
        ("codeberg.org", "/org/repo/archive/refs/tags/v1.2+3.tar.gz".into()),
        ("gitlab.com", "/api/v4/projects/group%2Fsubgroup%2Frepo%2Ename/repository/commits?ref_name=feature%2Fbranch".into()),
        ("gitlab.com", "/api/v4/projects/123/repository/archive.tar.gz?sha=refs%2Ftags%2Fv1.2%2B3".into()),
        ("gitlab.gnome.org", format!("/api/v4/projects/org%2Frepo/repository/archive.tar.gz?sha={REV}")),
        ("gitlab.freedesktop.org", "/api/v4/projects/org%2Frepo/repository/commits?ref_name=HEAD".into()),
        ("git.sr.ht", "/~org/repo/HEAD".into()),
        ("git.sr.ht", "/~org/repo/info/refs".into()),
        ("git.sr.ht", format!("/~org/repo/archive/{REV}.tar.gz")),
        ("git.sr.ht", "/~org/repo/archive/v1.2.tar.gz".into()),
    ] {
        let AuthorizedRequest::Http(read) = authorize(&policy, host, &Method::GET, &path).unwrap() else { panic!() };
        assert_eq!(read.url().as_str(), format!("https://{host}{path}"));
        assert!(authorize(&policy, host, &Method::POST, &path).is_err());
        assert!(authorize(&policy, host, &Method::DELETE, &path).is_err());
    }
    for (host, path) in denied_routes() {
        assert!(
            authorize(&policy, host, &Method::GET, path).is_err(),
            "accepted {host}{path}"
        );
    }
    // A plain Git policy must never inherit forge routes just because the hostname matches.
    let plain = Policy::new(Upstreams::from([(
        "codeberg.org".into(),
        UpstreamConfig::Git {},
    )]))
    .unwrap();
    assert!(
        authorize(
            &plain,
            "codeberg.org",
            &Method::GET,
            "/org/repo/archive/v1.tar.gz"
        )
        .is_err()
    );
}

fn denied_routes() -> Vec<(&'static str, &'static str)> {
    vec![
        ("codeberg.org", "/api/v1/repos/org/repo/issues"),
        ("codeberg.org", "/org/repo/archive/v1.tar.gz?delete=true"),
        ("codeberg.org", "/org/repo/archive/../v1.tar.gz"),
        ("codeberg.org", "/org/repo/archive/%2e%2e/v1.tar.gz"),
        ("codeberg.org", "/org/repo/archive/v1.tar.gz/extra"),
        ("codeberg.org", "/org/repo/archive/v1.zip"),
        (
            "gitlab.com",
            "/api/v4/projects/org%2Frepo/repository/files/file/raw?ref=HEAD",
        ),
        (
            "gitlab.com",
            "/api/v4/projects/org%2Frepo/repository/commits?ref_name=HEAD&ref_name=other",
        ),
        (
            "gitlab.com",
            "/api/v4/projects/org%2Frepo/repository/commits?ref_name=HEAD&method=DELETE",
        ),
        (
            "gitlab.com",
            "/api/v4/projects/org%2Frepo/repository/archive.tar.gz?sha=HEAD&path=subdir",
        ),
        (
            "gitlab.com",
            "/api/v4/projects/org%2Frepo/repository/archive.tar.gz?ref=HEAD",
        ),
        (
            "gitlab.com",
            "/api/v4/projects/org%2Frepo/repository/archive.tar.gz?sha=",
        ),
        (
            "gitlab.com",
            "/api/v4/projects/org%2Frepo/repository/archive.tar.gz?sha=..%2FHEAD",
        ),
        (
            "gitlab.com",
            "/api/v4/projects/org%2Frepo/repository/archive.tar.gz?sha=%0AHEAD",
        ),
        (
            "gitlab.com",
            "/api/v4/projects/org%2F..%2Frepo/repository/archive.tar.gz?sha=HEAD",
        ),
        (
            "gitlab.com",
            "/api/v4/projects/org%252Frepo/repository/archive.tar.gz?sha=HEAD",
        ),
        (
            "gitlab.com",
            "/api/v4/projects/org%2Frepo/repository/commits/HEAD",
        ),
        (
            "gitlab.com",
            "/api/v4/projects/org%2Frepo/repository/commits",
        ),
        ("git.sr.ht", "/~org/repo/refs/heads/main"),
        ("git.sr.ht", "/~org/repo/info/refs?service=git-receive-pack"),
        ("git.sr.ht", "/~org/repo/archive/v1.tar.gz?method=DELETE"),
        ("git.sr.ht", "/~org/repo/archive/../v1.tar.gz"),
        ("git.sr.ht", "/org/repo/archive/v1.tar.gz"),
    ]
}

#[test]
fn forge_redirects_cannot_change_policy_host_repository_or_selector() {
    let policy = Policy::new(upstreams()).unwrap();
    for (host, route) in [
        ("codeberg.org", "/org/repo/archive/v1.tar.gz"),
        (
            "gitlab.com",
            "/api/v4/projects/org%2Frepo/repository/archive.tar.gz?sha=v1",
        ),
        ("git.sr.ht", "/~org/repo/archive/v1.tar.gz"),
        (
            "gitlab.com",
            "/api/v4/projects/org%2Frepo/repository/commits?ref_name=HEAD",
        ),
        ("git.sr.ht", "/~org/repo/HEAD"),
    ] {
        let read = || match authorize(&policy, host, &Method::GET, route).unwrap() {
            AuthorizedRequest::Http(read) => read,
            _ => panic!(),
        };
        assert!(read().redirect(&policy, route).is_ok());
        for location in [
            route.replace("v1", "v2"),
            route.replace("org", "other"),
            format!("http://{host}{route}"),
            format!("https://evil.com{route}"),
            format!("https://gitlab.gnome.org{route}"),
            format!("https://{host}:444{route}"),
            format!("{route}#fragment"),
            format!("/org/repo/archive/{REV}.tar.gz"),
        ] {
            if location == route {
                continue;
            }
            assert!(
                read().redirect(&policy, &location).is_err(),
                "followed {location}"
            );
        }
    }
    let AuthorizedRequest::Http(github) = authorize(
        &policy,
        "github.com",
        &Method::GET,
        &format!("/org/repo/archive/{REV}.tar.gz"),
    )
    .unwrap() else {
        panic!()
    };
    assert!(
        github
            .redirect(
                &policy,
                &format!("https://codeberg.org/org/repo/archive/{REV}.tar.gz")
            )
            .is_err()
    );
}

#[tokio::test]
async fn forge_tls_reads_preserve_selectors_and_denials_never_reach_upstream() {
    let origin = Origin::new().await;
    let gateway = origin.gateway();
    for (host, path) in denied_routes() {
        let request = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\n\r\n");
        let reply = origin
            .exchange(gateway.clone(), host, host, request.as_bytes())
            .await
            .unwrap();
        assert!(reply.starts_with(b"HTTP/1.1 4"), "accepted {host}{path}");
    }
    for host in ["codeberg.org", "gitlab.com", "git.sr.ht"] {
        let request = format!(
            "POST /org/repo.git/git-receive-pack HTTP/1.1\r\nHost: {host}\r\nContent-Length: 999999999\r\nExpect: 100-continue\r\n\r\n"
        );
        assert!(
            origin
                .exchange(gateway.clone(), host, host, request.as_bytes())
                .await
                .unwrap()
                .starts_with(b"HTTP/1.1 4")
        );
    }
    assert!(origin.requests.lock().unwrap().is_empty());
    for (host, path) in [
        ("codeberg.org", "/org/repo/archive/refs/tags/v1.2.tar.gz"),
        (
            "gitlab.gnome.org",
            "/api/v4/projects/org%2Frepo%2Ename/repository/archive.tar.gz?sha=refs%2Ftags%2Fv1.2%2B3",
        ),
        ("git.sr.ht", "/~org/repo/archive/v1.2.tar.gz"),
        ("git.sr.ht", "/~org/repo/HEAD"),
    ] {
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: {host}\r\nAuthorization: secret\r\nPrivate-Token: secret\r\nCookie: secret\r\nRange: bytes=0-10\r\n\r\n"
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
        let records = origin.requests.lock().unwrap();
        let last = records.last().unwrap();
        assert_eq!((&*last.0, &*last.2), (host, path));
        assert_eq!(last.3["range"], "bytes=0-10");
        for header in ["authorization", "private-token", "cookie"] {
            assert!(!last.3.contains_key(header));
        }
    }
}
