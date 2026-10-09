use super::*;
use std::{
    fs,
    path::Path,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

fn nix(work: &Path) -> Command {
    let mut command = Command::new("nix");
    command
        .args(["--extra-experimental-features", "nix-command"])
        .env("XDG_CONFIG_HOME", work.join("config"))
        .env("XDG_CACHE_HOME", work.join("cache"));
    command
}

fn run(command: &mut Command) -> Vec<u8> {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

#[tokio::test]
async fn unmodified_nix_imports_a_binary_cache_object_through_connect() {
    if Command::new("nix").arg("--version").output().is_err() {
        assert!(
            std::env::var_os("NIX_TEST_REQUIRED").is_none(),
            "nix is required in the Linux image build"
        );
        return;
    }
    let work = std::env::temp_dir().join(format!(
        "ggez-nix-cache-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&work).unwrap();
    fs::write(work.join("value"), b"through the cache gateway\n").unwrap();
    let store_path = String::from_utf8(run(nix(&work)
        .arg("--store")
        .arg(work.join("source-store"))
        .args(["store", "add-path"])
        .arg(work.join("value"))))
    .unwrap()
    .trim()
    .to_owned();
    let nar = run(nix(&work)
        .arg("--store")
        .arg(work.join("source-store"))
        .args(["store", "dump-path", &store_path]));
    fs::write(work.join("archive.nar"), &nar).unwrap();
    let hash = String::from_utf8(run(nix(&work)
        .args(["hash", "file", "--type", "sha256", "--base32"])
        .arg(work.join("archive.nar"))))
    .unwrap();
    let store_name = store_path.rsplit('/').next().unwrap();
    let origin = Origin::with_cache(Cache {
        narinfo_path: format!("/{}.narinfo", &store_name[..32]),
        narinfo: format!(
            "StorePath: {store_path}\nURL: nar/archive.nar\nCompression: none\nNarHash: sha256:{}\nNarSize: {}\nReferences: \n",
            hash.trim(), nar.len()
        )
        .into_bytes(),
        nar,
    })
    .await;
    fs::write(work.join("ca.crt"), &origin.ca).unwrap();
    let gateway = origin.gateway();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            tokio::spawn(serve(stream, gateway.clone(), Duration::from_secs(30)));
        }
    });
    let destination = work.join("destination-store");
    let physical_path = destination.join(store_path.trim_start_matches('/'));
    let copy_work = work.clone();
    let output = tokio::task::spawn_blocking(move || {
        nix(&copy_work)
            .args(["copy", "--from", "https://cache.nixos.org", "--to"])
            .arg(destination)
            .args(["--no-check-sigs", &store_path])
            .env("https_proxy", proxy)
            .env("NIX_SSL_CERT_FILE", copy_work.join("ca.crt"))
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
    let content = fs::read(physical_path);
    fs::remove_dir_all(work).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(content.unwrap(), b"through the cache gateway\n");
    let requests = origin.requests.lock().unwrap();
    assert!(requests.iter().any(|r| r.2 == "/nix-cache-info"));
    assert!(requests.iter().any(|r| r.2.ends_with(".narinfo")));
    assert!(requests.iter().any(|r| r.2 == "/nar/archive.nar"));
    assert!(
        requests
            .iter()
            .all(|r| r.0 == "cache.nixos.org" && (r.1 == Method::GET || r.1 == Method::HEAD))
    );
}
