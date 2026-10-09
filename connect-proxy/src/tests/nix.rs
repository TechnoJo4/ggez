use super::*;
use std::{
    fs,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

#[tokio::test]
async fn unmodified_nix_resolves_and_downloads_github_through_connect() {
    if Command::new("nix").arg("--version").output().is_err() {
        assert!(
            std::env::var_os("NIX_TEST_REQUIRED").is_none(),
            "nix is required in the Linux image build"
        );
        eprintln!("nix unavailable; run the Dockerfile's Linux tests");
        return;
    }
    let origin = Origin::new().await;
    let gateway = origin.gateway();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let gateway = gateway.clone();
            tokio::spawn(serve(stream, gateway, Duration::from_secs(30)));
        }
    });
    let path = std::env::temp_dir().join(format!(
        "ggez-nix-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&path).unwrap();
    fs::write(path.join("ca.crt"), &origin.ca).unwrap();
    let work = path.clone();
    let output = tokio::task::spawn_blocking(move || {
        Command::new("nix")
            .args([
                "--extra-experimental-features",
                "nix-command flakes",
                "--store",
            ])
            .arg(work.join("store"))
            .args([
                "--option",
                "sandbox",
                "false",
                "--option",
                "flake-registry",
                "",
                "eval",
                "--raw",
                "github:org/repo#value",
            ])
            .env("https_proxy", proxy)
            .env("NIX_SSL_CERT_FILE", work.join("ca.crt"))
            .env("XDG_CACHE_HOME", work.join("cache"))
            .env("XDG_CONFIG_HOME", work.join("config"))
            .env_remove("HTTPS_PROXY")
            .env_remove("ALL_PROXY")
            .env_remove("all_proxy")
            .env_remove("NO_PROXY")
            .env_remove("no_proxy")
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    task.abort();
    fs::remove_dir_all(path).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"through the gateway");
    let recorded = origin.requests.lock().unwrap();
    assert!(
        recorded
            .iter()
            .any(|r| r.0 == "api.github.com" && r.2 == "/repos/org/repo/commits/HEAD")
    );
    assert!(
        recorded
            .iter()
            .any(|r| r.0 == "github.com" && r.2.contains("/archive/"))
    );
    assert!(recorded.iter().any(|r| r.0 == "codeload.github.com"));
    assert!(recorded.iter().all(|r| r.1 == Method::GET));
}
