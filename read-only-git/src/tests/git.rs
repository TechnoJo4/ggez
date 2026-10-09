use super::*;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "ggez-git-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn git_command(dir: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_CONFIG_COUNT")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("HTTP_PROXY")
        .env_remove("HTTPS_PROXY")
        .env_remove("ALL_PROXY")
        .env_remove("http_proxy")
        .env_remove("https_proxy")
        .env_remove("all_proxy");
    command
}

fn git(dir: &Path, args: &[&str]) -> Output {
    let output = git_command(dir)
        .args(args)
        .output()
        .expect("git is required for the smart HTTP integration test");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn backend(root: &Path, request: &Recorded) -> Response<Full<Bytes>> {
    let mut command = git_command(root);
    command
        .arg("http-backend")
        .env("GIT_PROJECT_ROOT", root)
        .env("GIT_HTTP_EXPORT_ALL", "1")
        .env(
            "PATH_INFO",
            request.uri.path().strip_prefix("/backend").unwrap(),
        )
        .env("REQUEST_METHOD", request.method.as_str())
        .env("QUERY_STRING", request.uri.query().unwrap_or(""))
        .env("CONTENT_LENGTH", request.body.len().to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (header, variable) in [
        ("content-type", "CONTENT_TYPE"),
        ("content-encoding", "HTTP_CONTENT_ENCODING"),
        ("git-protocol", "GIT_PROTOCOL"),
    ] {
        if let Some(value) = request.headers.get(header) {
            command.env(variable, value.to_str().unwrap());
        }
    }
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&request.body)
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "http-backend failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let split = output
        .stdout
        .windows(4)
        .position(|s| s == b"\r\n\r\n")
        .unwrap();
    let mut response = Response::new(Full::new(Bytes::copy_from_slice(
        &output.stdout[split + 4..],
    )));
    for line in std::str::from_utf8(&output.stdout[..split])
        .unwrap()
        .split("\r\n")
    {
        let (name, value) = line.split_once(": ").unwrap();
        if name.eq_ignore_ascii_case("status") {
            *response.status_mut() =
                StatusCode::from_bytes(value.split(' ').next().unwrap().as_bytes()).unwrap();
        } else {
            response.headers_mut().insert(
                hyper::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                value.parse().unwrap(),
            );
        }
    }
    response
}

struct HttpBridge {
    url: String,
    task: JoinHandle<()>,
}
impl HttpBridge {
    async fn new(gateway: Arc<Gateway>) -> Self {
        // Production accepts UDS; this test-only TCP adapter gives an unmodified
        // git executable the same HTTP server, without requiring vsock.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let gateway = gateway.clone();
                tokio::spawn(async move {
                    let _ = serve(stream, gateway, Duration::from_secs(30)).await;
                });
            }
        });
        Self { url, task }
    }
}
impl Drop for HttpBridge {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn seed(root: &Path) {
    git(root, &["init", "-q", "-b", "main", "work"]);
    let work = root.join("work");
    fs::write(
        work.join("payload"),
        (0..100_000).map(|n| n as u8).collect::<Vec<_>>(),
    )
    .unwrap();
    git(&work, &["add", "payload"]);
    git(
        &work,
        &[
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-qm",
            "seed",
        ],
    );
    fs::create_dir(root.join("group")).unwrap();
    git(root, &["clone", "-q", "--bare", "work", "group/repo.git"]);
    let bare = root.join("group/repo.git");
    git(&bare, &["config", "http.receivepack", "true"]);
    git(&bare, &["config", "uploadpack.allowFilter", "true"]);
}

#[tokio::test]
async fn real_git_clones_fetches_and_cannot_push_to_a_writable_upstream() {
    let fixture = Arc::new(Fixture::new());
    let setup = fixture.clone();
    tokio::task::spawn_blocking(move || seed(&setup.0))
        .await
        .unwrap();
    let root = fixture.clone();
    let origin = Origin::new(move |request| backend(&root.0, request)).await;
    let bridge = HttpBridge::new(gateway(&[("github", &format!("{}/backend/", origin.url))])).await;
    let rewrite = format!("url.{}/github/.insteadOf=https://github.com/", bridge.url);
    let direct_upstream = format!("{}/backend/group/repo.git", origin.url);
    let work = fixture.clone();
    tokio::task::spawn_blocking(move || {
        for version in ["0", "1", "2"] {
            let directory = format!("clone-v{version}");
            let protocol = format!("protocol.version={version}");
            git(
                &work.0,
                &[
                    "-c",
                    &rewrite,
                    "-c",
                    &protocol,
                    "-c",
                    "http.postBuffer=65536",
                    "clone",
                    "-q",
                    "https://github.com/group/repo.git",
                    &directory,
                ],
            );
            let cloned = work.0.join(directory);
            assert_eq!(
                fs::read(cloned.join("payload")).unwrap(),
                fs::read(work.0.join("work/payload")).unwrap()
            );
            git(
                &cloned,
                &[
                    "config",
                    rewrite.split_once('=').unwrap().0,
                    "https://github.com/",
                ],
            );
            git(&cloned, &["config", "protocol.version", version]);
            git(&cloned, &["config", "http.postBuffer", "65536"]);
        }
        git(
            &work.0,
            &[
                "-c",
                &rewrite,
                "clone",
                "-q",
                "--depth=1",
                "https://github.com/group/repo.git",
                "shallow",
            ],
        );
        git(
            &work.0,
            &[
                "-c",
                &rewrite,
                "clone",
                "-q",
                "--filter=blob:none",
                "https://github.com/group/repo.git",
                "partial",
            ],
        );
        let source = work.0.join("work");
        fs::write(source.join("new-file"), b"a later commit\n").unwrap();
        git(&source, &["add", "new-file"]);
        git(
            &source,
            &[
                "-c",
                "user.name=fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "-qm",
                "later",
            ],
        );
        // Positive control: this same HTTP upstream really does accept a push.
        git(&source, &["push", "-q", &direct_upstream, "main"]);
        for version in ["0", "1", "2"] {
            let cloned = work.0.join(format!("clone-v{version}"));
            git(&cloned, &["fetch", "-q"]);
            assert_eq!(
                git(&cloned, &["rev-parse", "origin/main"]).stdout,
                git(&source, &["rev-parse", "HEAD"]).stdout
            );
        }
    })
    .await
    .unwrap();
    {
        let requests = origin.requests.lock().unwrap();
        assert!(requests.iter().any(|r| r.method == Method::GET));
        assert!(requests.iter().any(|r| r.method == Method::POST));
        assert!(requests.iter().any(|r| {
            r.headers
                .get("git-protocol")
                .is_some_and(|v| v == "version=1")
        }));
        assert!(requests.iter().any(|r| {
            r.headers
                .get("git-protocol")
                .is_some_and(|v| v == "version=2")
        }));
        assert!(
            requests
                .iter()
                .all(|r| r.uri.path().starts_with("/backend/group/repo.git/"))
        );
        assert!(
            requests
                .iter()
                .any(|r| r.method == Method::POST && r.uri.path().ends_with("/git-receive-pack")),
            "the positive control did not push over HTTP"
        );
    }
    let before = origin.requests.lock().unwrap().len();
    let root = fixture.clone();
    tokio::task::spawn_blocking(move || {
        let bare = root.0.join("group/repo.git");
        let upstream_tip = git(&bare, &["rev-parse", "main"]).stdout;
        for version in ["0", "1", "2"] {
            let cloned = root.0.join(format!("clone-v{version}"));
            let output = git_command(&cloned)
                .args(["push", "origin", "HEAD:refs/heads/forbidden"])
                .output()
                .unwrap();
            assert!(!output.status.success(), "push unexpectedly succeeded");
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("403"),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        assert_eq!(git(&bare, &["rev-parse", "main"]).stdout, upstream_tip);
        assert!(
            !git_command(&bare)
                .args(["show-ref", "--verify", "refs/heads/forbidden"])
                .output()
                .unwrap()
                .status
                .success()
        );
    })
    .await
    .unwrap();
    assert_eq!(
        origin.requests.lock().unwrap().len(),
        before,
        "push contacted the upstream"
    );
}
