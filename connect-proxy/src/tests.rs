use crate::{
    client::Client,
    policy::{AuthorizedRequest, Policy, UpstreamConfig, Upstreams},
    server::{Gateway, connect_host, serve},
    tls,
};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{
    HeaderMap, Method, Request, Response, StatusCode, server::conn::http1, service::service_fn,
};
use hyper_util::rt::TokioIo;
use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair, KeyUsagePurpose};
use rustls::{ClientConfig, RootCertStore, pki_types::ServerName};
use std::{
    convert::Infallible,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, duplex},
    net::TcpListener,
    task::JoinHandle,
};
use tokio_rustls::TlsConnector;

mod forges;
#[cfg(unix)]
mod nix;

const REV: &str = "0123456789012345678901234567890123456789";

fn archive() -> Vec<u8> {
    let gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut tar = tar::Builder::new(gzip);
    let flake = b"{ outputs = { self }: { value = \"through the gateway\"; }; }\n";
    let mut header = tar::Header::new_gnu();
    header.set_size(flake.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(1);
    header.set_cksum();
    tar.append_data(&mut header, "repo/flake.nix", &flake[..])
        .unwrap();
    tar.into_inner().unwrap().finish().unwrap()
}

fn ca() -> (String, String) {
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::default();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    (params.self_signed(&key).unwrap().pem(), key.serialize_pem())
}

fn upstreams() -> Upstreams {
    Upstreams::from([
        ("github.com".into(), UpstreamConfig::Github {}),
        ("api.github.com".into(), UpstreamConfig::GithubApi {}),
        (
            "codeload.github.com".into(),
            UpstreamConfig::GithubCodeload {},
        ),
        ("gitlab.com".into(), UpstreamConfig::Gitlab {}),
        ("gitlab.freedesktop.org".into(), UpstreamConfig::Gitlab {}),
        ("gitlab.gnome.org".into(), UpstreamConfig::Gitlab {}),
        ("codeberg.org".into(), UpstreamConfig::Forgejo {}),
        ("git.sr.ht".into(), UpstreamConfig::Sourcehut {}),
    ])
}

#[test]
fn connect_only_admits_exact_whitelisted_authorities_without_bodies() {
    let policy = Policy::new(upstreams()).unwrap();
    assert_eq!(connect_host(b"CONNECT github.com:443 HTTP/1.1\r\nHost: github.com:443\r\nUser-Agent: curl\r\nProxy-Connection: Keep-Alive\r\n\r\n", &policy).unwrap(), "github.com");
    for head in [
        "CONNECT github.com:443 HTTP/1.1\r\nHost: evil:443\r\n\r\n",
        "CONNECT github.com:443 HTTP/1.1\r\nHost: github.com:443\r\nHost: github.com:443\r\n\r\n",
        "CONNECT github.com:443 HTTP/1.1\r\nHost: github.com:443\r\nContent-Length: 1\r\n\r\n",
        "CONNECT github.com:443 HTTP/1.1\r\nHost: github.com:443\r\nTransfer-Encoding: chunked\r\n\r\n",
        "CONNECT github.com:80 HTTP/1.1\r\nHost: github.com:80\r\n\r\n",
        "CONNECT evil.com:443 HTTP/1.1\r\nHost: evil.com:443\r\n\r\n",
        "CONNECT 127.0.0.1:443 HTTP/1.1\r\nHost: 127.0.0.1:443\r\n\r\n",
        "CONNECT https://github.com:443 HTTP/1.1\r\nHost: github.com:443\r\n\r\n",
        "CONNECT github.com:443 HTTP/1.0\r\nHost: github.com:443\r\n\r\n",
        "GET / HTTP/1.1\r\nHost: github.com:443\r\n\r\n",
    ] {
        assert!(
            connect_host(head.as_bytes(), &policy).is_err(),
            "accepted {head}"
        );
    }
    for host in [
        "localhost",
        "127.0.0.1",
        "UPPER.example",
        "x..example",
        "-x.example",
        "x.example:443",
        "x.example/path",
    ] {
        assert!(Policy::new(Upstreams::from([(host.into(), UpstreamConfig::Git {})])).is_err());
    }
    assert!(serde_json::from_str::<Upstreams>(r#"{"github.com":{"policy":"blind"}}"#).is_err());
    assert!(
        serde_json::from_str::<Upstreams>(r#"{"github.com":{"policy":"git","anything":true}}"#)
            .is_err()
    );
}

#[test]
fn forge_reads_and_redirects_have_an_explicit_operation_and_destination() {
    let policy = Policy::new(upstreams()).unwrap();
    let read = |host, path: &str| {
        policy.authorize(
            host,
            &Method::GET,
            &path.parse().unwrap(),
            &HeaderMap::new(),
            true,
        )
    };
    assert!(read("api.github.com", "/repos/org/repo/commits/feature/branch").is_ok());
    for (host, path) in [
        ("github.com", format!("/org/repo/archive/{REV}.tar.gz")),
        ("api.github.com", format!("/repos/org/repo/tarball/{REV}")),
        ("codeload.github.com", format!("/org/repo/tar.gz/{REV}")),
        (
            "codeload.github.com",
            format!("/org/repo/legacy.tar.gz/{REV}"),
        ),
    ] {
        assert!(read(host, &path).is_ok(), "rejected {host}{path}");
        assert!(read(host, &format!("{path}/extra")).is_err());
    }
    assert!(read("gitlab.com", &format!("/org/repo/archive/{REV}.tar.gz")).is_err());
    for path in [
        "/user",
        "/repos/org/repo/issues",
        "/repos/org/repo/commits",
        "/repos/org/repo/commits/HEAD?method=delete",
        "/repos/org/repo/commits/%2e%2e",
        "/repos/org/repo/commits//HEAD",
        "https://evil.com/repos/org/repo/commits/HEAD",
    ] {
        assert!(read("api.github.com", path).is_err(), "accepted {path}");
    }
    let archive = format!("/org/repo/archive/{REV}.tar.gz");
    let AuthorizedRequest::Forge(request) = read("github.com", &archive).unwrap() else {
        panic!()
    };
    assert!(
        request
            .redirect(
                &policy,
                &format!("https://codeload.github.com/org/repo/tar.gz/{REV}")
            )
            .is_ok()
    );
    for location in [
        format!("http://codeload.github.com/org/repo/tar.gz/{REV}"),
        format!("https://evil.com/org/repo/tar.gz/{REV}"),
        format!("https://codeload.github.com/other/repo/tar.gz/{REV}"),
        "https://codeload.github.com/org/repo/tar.gz/ffffffffffffffffffffffffffffffffffffffff"
            .into(),
        format!("https://codeload.github.com/org/repo/tar.gz/{REV}?x=1"),
        format!("https://user:secret@codeload.github.com/org/repo/tar.gz/{REV}"),
        format!("https://codeload.github.com:444/org/repo/tar.gz/{REV}"),
        "https://api.github.com/repos/org/repo/commits/HEAD".into(),
    ] {
        let AuthorizedRequest::Forge(request) = read("github.com", &archive).unwrap() else {
            panic!()
        };
        assert!(
            request.redirect(&policy, &location).is_err(),
            "followed {location}"
        );
    }
}

type Recorded = (String, Method, String, HeaderMap, Bytes);

struct Origin {
    address: std::net::SocketAddr,
    requests: Arc<Mutex<Vec<Recorded>>>,
    task: JoinHandle<()>,
    ca: String,
    key: String,
}

impl Drop for Origin {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Origin {
    async fn new() -> Self {
        let (ca, key) = ca();
        let policy = Policy::new(upstreams()).unwrap();
        let acceptors = Arc::new(tls::acceptors(&ca, &key, policy.hosts()).unwrap());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let task = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let recorded = recorded.clone();
                let acceptors = acceptors.clone();
                tokio::spawn(async move {
                    let lazy = tokio_rustls::LazyConfigAcceptor::new(
                        rustls::server::Acceptor::default(),
                        stream,
                    );
                    let hello = lazy.await.unwrap();
                    let host = hello.client_hello().server_name().unwrap().to_owned();
                    let stream = hello
                        .into_stream(acceptors[&host].config().clone())
                        .await
                        .unwrap();
                    let service = service_fn(move |request: Request<hyper::body::Incoming>| {
                        let recorded = recorded.clone();
                        let host = host.clone();
                        async move {
                            let (parts, body) = request.into_parts();
                            let body = body.collect().await.unwrap().to_bytes();
                            let path = parts.uri.to_string();
                            recorded.lock().unwrap().push((
                                host.clone(),
                                parts.method.clone(),
                                path.clone(),
                                parts.headers,
                                body,
                            ));
                            let mut response = Response::new(Full::new(Bytes::new()));
                            if host == "api.github.com" {
                                *response.body_mut() = Full::new(Bytes::from(format!(
                                    r#"{{"sha":"{REV}","commit":{{"tree":{{"sha":"{REV}"}}}}}}"#
                                )));
                                response
                                    .headers_mut()
                                    .insert("content-type", "application/json".parse().unwrap());
                            } else if host.starts_with("gitlab.")
                                && path.contains("/repository/commits?")
                            {
                                *response.body_mut() =
                                    Full::new(Bytes::from(format!(r#"[{{"id":"{REV}"}}]"#)));
                                response
                                    .headers_mut()
                                    .insert("content-type", "application/json".parse().unwrap());
                            } else if host == "github.com" && path.contains("/archive/") {
                                *response.status_mut() = StatusCode::FOUND;
                                response.headers_mut().insert(
                                    "location",
                                    format!("https://codeload.github.com/org/repo/tar.gz/{REV}")
                                        .parse()
                                        .unwrap(),
                                );
                            } else if path.contains("/tar.gz/")
                                || path.contains("/archive/")
                                || path.contains("/repository/archive.tar.gz?")
                            {
                                *response.body_mut() = Full::new(Bytes::from(archive()));
                            } else if host == "git.sr.ht" && path == "/~org/repo/HEAD" {
                                *response.body_mut() =
                                    Full::new(Bytes::from_static(b"ref: refs/heads/main\n"));
                            } else if host == "git.sr.ht" && path == "/~org/repo/info/refs" {
                                *response.body_mut() =
                                    Full::new(Bytes::from(format!("{REV}\trefs/heads/main\n")));
                            } else {
                                *response.body_mut() = Full::new(Bytes::from_static(b"0000"));
                                response.headers_mut().insert(
                                    "content-type",
                                    if parts.method == Method::GET {
                                        "application/x-git-upload-pack-advertisement"
                                    } else {
                                        "application/x-git-upload-pack-result"
                                    }
                                    .parse()
                                    .unwrap(),
                                );
                            }
                            response
                                .headers_mut()
                                .insert("set-cookie", "host-secret".parse().unwrap());
                            Ok::<_, Infallible>(response)
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
            address,
            requests,
            task,
            ca,
            key,
        }
    }

    fn gateway(&self) -> Arc<Gateway> {
        let policy = Policy::new(upstreams()).unwrap();
        let mut builder = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .add_root_certificate(reqwest::Certificate::from_pem(self.ca.as_bytes()).unwrap());
        for host in policy.hosts() {
            builder = builder.resolve(host, self.address);
        }
        let http = builder.build().unwrap();
        Arc::new(Gateway {
            acceptors: tls::acceptors(&self.ca, &self.key, policy.hosts()).unwrap(),
            policy,
            client: Client::for_test(http),
            max_body: 32,
        })
    }

    async fn exchange(
        &self,
        gateway: Arc<Gateway>,
        host: &str,
        sni: &str,
        request: &[u8],
    ) -> Result<Vec<u8>, String> {
        let (mut stream, server) = duplex(16384);
        let task = tokio::spawn(serve(server, gateway, Duration::from_secs(5)));
        stream
            .write_all(
                format!("CONNECT {host}:443 HTTP/1.1\r\nHost: {host}:443\r\n\r\n").as_bytes(),
            )
            .await
            .unwrap();
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            head.push(stream.read_u8().await.unwrap());
        }
        assert!(head.starts_with(b"HTTP/1.1 200"));
        let mut roots = RootCertStore::empty();
        for cert in rustls_pemfile::certs(&mut self.ca.as_bytes()) {
            roots.add(cert.unwrap()).unwrap();
        }
        let config =
            ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_root_certificates(roots)
                .with_no_client_auth();
        let tls = TlsConnector::from(Arc::new(config))
            .connect(ServerName::try_from(sni.to_owned()).unwrap(), stream)
            .await;
        let mut tls = match tls {
            Ok(tls) => tls,
            Err(error) => {
                let _ = task.await;
                return Err(error.to_string());
            }
        };
        tls.write_all(request).await.unwrap();
        let mut bytes = Vec::new();
        tls.read_to_end(&mut bytes).await.unwrap();
        task.await.unwrap().unwrap();
        Ok(bytes)
    }
}

#[tokio::test]
async fn tls_enforces_read_policy_and_never_contacts_upstream_for_writes() {
    let origin = Origin::new().await;
    for line in [
        "POST /org/repo.git/git-receive-pack",
        "GET /org/repo.git/info/refs?service=git-receive-pack",
        "DELETE /org/repo",
        "GET https://evil.com/org/repo.git/info/refs?service=git-upload-pack",
        "GET /org/repo/archive/main.tar.gz",
    ] {
        let request = format!(
            "{line} HTTP/1.1\r\nHost: github.com\r\nContent-Length: 999999999\r\nExpect: 100-continue\r\n\r\n"
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
        assert!(reply.starts_with(b"HTTP/1.1 4"));
        assert!(origin.requests.lock().unwrap().is_empty());
    }
    let reply = origin.exchange(origin.gateway(), "github.com", "github.com", b"GET /org/repo.git/info/refs?service=git-upload-pack HTTP/1.1\r\nHost: api.github.com\r\n\r\n").await.unwrap();
    assert!(reply.starts_with(b"HTTP/1.1 400"));
    assert!(
        origin
            .exchange(origin.gateway(), "github.com", "api.github.com", b"")
            .await
            .is_err()
    );
    assert!(origin.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn tls_reads_preserve_git_negotiation_and_nix_forge_downloads() {
    let origin = Origin::new().await;
    let requests = [
        ("github.com", "GET /org/repo.git/info/refs?service=git-upload-pack HTTP/1.1\r\nHost: github.com\r\nGit-Protocol: version=2\r\nCookie: guest-secret\r\nAuthorization: guest-secret\r\nX-HTTP-Method-Override: DELETE\r\n\r\n".to_owned()),
        ("gitlab.com", "POST /org/repo.git/git-upload-pack HTTP/1.1\r\nHost: gitlab.com\r\nContent-Type: application/x-git-upload-pack-request\r\nContent-Length: 4\r\n\r\n0000".to_owned()),
        ("api.github.com", "GET /repos/org/repo/commits/HEAD HTTP/1.1\r\nHost: api.github.com\r\nAuthorization: guest-secret\r\n\r\n".to_owned()),
        ("github.com", format!("GET /org/repo/archive/{REV}.tar.gz HTTP/1.1\r\nHost: github.com\r\n\r\n")),
    ];
    for (host, request) in requests {
        let reply = origin
            .exchange(origin.gateway(), host, host, request.as_bytes())
            .await
            .unwrap();
        assert!(
            reply.starts_with(b"HTTP/1.1 200"),
            "{host}: {}",
            String::from_utf8_lossy(&reply)
        );
        assert!(!String::from_utf8_lossy(&reply).contains("host-secret"));
    }
    let recorded = origin.requests.lock().unwrap();
    assert_eq!(recorded.len(), 5);
    assert_eq!(recorded[0].3["git-protocol"], "version=2");
    assert_eq!(recorded[1].4, b"0000".as_slice());
    assert_eq!(recorded[4].0, "codeload.github.com");
    for request in recorded.iter() {
        for name in ["authorization", "cookie", "x-http-method-override"] {
            assert!(!request.3.contains_key(name));
        }
    }
}

#[test]
fn ca_key_and_constraints_are_checked_before_serving() {
    let (certificate, key) = ca();
    let (_, wrong_key) = ca();
    assert!(tls::acceptors(&certificate, &wrong_key, ["github.com"].into_iter()).is_err());
    let leaf = CertificateParams::new(vec!["github.com".into()])
        .unwrap()
        .self_signed(&KeyPair::from_pem(&key).unwrap())
        .unwrap()
        .pem();
    assert!(tls::acceptors(&leaf, &key, ["github.com"].into_iter()).is_err());
    assert!(tls::acceptors(&certificate, &key, ["github.com"].into_iter()).is_ok());
}
