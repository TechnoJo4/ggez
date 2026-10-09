use super::*;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::{SystemTime, UNIX_EPOCH},
};

fn available(program: &str, args: &[&str]) -> bool {
    let available = Command::new(program)
        .args(args)
        .output()
        .is_ok_and(|output| output.status.success());
    assert!(
        available || std::env::var_os("PACKAGE_TEST_REQUIRED").is_none(),
        "{program} {args:?} is required in the Linux image build"
    );
    available
}

fn checked(command: &mut Command) -> Output {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn command(program: &str, work: &Path) -> Command {
    let mut command = Command::new(program);
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("HOME", work)
        .current_dir(work);
    if let Some(rustup) = std::env::var_os("RUSTUP_HOME") {
        command.env("RUSTUP_HOME", rustup);
    }
    command
}

struct PackageClient {
    work: PathBuf,
    task: Option<JoinHandle<()>>,
}

impl PackageClient {
    fn new() -> Self {
        let work = std::env::temp_dir().join(format!(
            "ggez-packages-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&work).unwrap();
        Self { work, task: None }
    }

    async fn proxy(&mut self, origin: &Origin) -> String {
        fs::write(self.work.join("ca.crt"), &origin.ca).unwrap();
        let gateway = origin.gateway();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy = format!("http://{}", listener.local_addr().unwrap());
        self.task = Some(tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                tokio::spawn(serve(stream, gateway.clone(), Duration::from_secs(30)));
            }
        }));
        proxy
    }
}

impl Drop for PackageClient {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
        let _ = fs::remove_dir_all(&self.work);
    }
}

#[tokio::test]
async fn unmodified_pip_installs_a_wheel_and_reads_its_metadata_through_connect() {
    if !available("python3", &["-m", "pip", "--version"]) {
        return;
    }
    let mut client = PackageClient::new();
    let output = checked(command("python3", &client.work).args(["-c", r#"
import hashlib, json, pathlib, zipfile
metadata = b'Metadata-Version: 2.1\nName: ggez-fixture\nVersion: 1.0.0+local\n\n'
wheel = pathlib.Path('ggez_fixture-1.0.0+local-py3-none-any.whl')
with zipfile.ZipFile(wheel, 'w') as archive:
    archive.writestr('ggez_fixture.py', 'value = "through the package gateway"\n')
    archive.writestr('ggez_fixture-1.0.0+local.dist-info/METADATA', metadata)
    archive.writestr('ggez_fixture-1.0.0+local.dist-info/WHEEL', 'Wheel-Version: 1.0\nRoot-Is-Purelib: true\nTag: py3-none-any\n')
    archive.writestr('ggez_fixture-1.0.0+local.dist-info/RECORD', '')
pathlib.Path('metadata').write_bytes(metadata)
print(json.dumps([hashlib.sha256(wheel.read_bytes()).hexdigest(), hashlib.sha256(metadata).hexdigest()]))
"#]));
    let hashes: Vec<String> = serde_json::from_slice(&output.stdout).unwrap();
    let file = format!(
        "/packages/aa/bb/{}/ggez_fixture-1.0.0+local-py3-none-any.whl",
        "c".repeat(60)
    );
    let encoded_file = file.replace('+', "%2B");
    let origin = Origin::with_routes(HashMap::from([
        (("pypi.org".into(), "/simple/ggez-fixture/".into()), (
            "text/html".into(),
            format!(r#"<!doctype html><a href="https://files.pythonhosted.org{file}#sha256={}" data-core-metadata="sha256={}" data-dist-info-metadata="sha256={}">ggez_fixture-1.0.0+local-py3-none-any.whl</a>"#, hashes[0], hashes[1], hashes[1]).into_bytes(),
        )),
        (("files.pythonhosted.org".into(), encoded_file.clone()), (
            "application/octet-stream".into(), fs::read(client.work.join("ggez_fixture-1.0.0+local-py3-none-any.whl")).unwrap(),
        )),
        (("files.pythonhosted.org".into(), format!("{encoded_file}.metadata")), (
            "application/octet-stream".into(), fs::read(client.work.join("metadata")).unwrap(),
        )),
    ])).await;
    let proxy = client.proxy(&origin).await;
    let work = client.work.clone();
    tokio::task::spawn_blocking(move || {
        checked(command("python3", &work)
            .args(["-m", "pip", "install", "--no-deps", "--no-cache-dir", "--disable-pip-version-check", "--target", "installed", "ggez-fixture==1.0.0+local"])
            .env("HTTPS_PROXY", proxy)
            .env("PIP_CERT", work.join("ca.crt"))
            .env("PIP_CONFIG_FILE", "/dev/null")
            .env("PIP_RETRIES", "0")
            .env("PIP_TIMEOUT", "5"));
        let output = checked(command("python3", &work).args(["-c", "import sys; sys.path.insert(0, 'installed'); import ggez_fixture; print(ggez_fixture.value)"]));
        assert_eq!(output.stdout, b"through the package gateway\n");
    }).await.unwrap();
    let requests = origin.requests.lock().unwrap();
    assert!(
        requests
            .iter()
            .any(|r| r.0 == "pypi.org" && r.2 == "/simple/ggez-fixture/")
    );
    for path in [&encoded_file, &format!("{encoded_file}.metadata")] {
        assert!(
            requests
                .iter()
                .any(|r| r.0 == "files.pythonhosted.org" && &r.2 == path)
        );
    }
    assert!(
        requests
            .iter()
            .all(|r| r.1 == Method::GET && r.4.is_empty())
    );
}

#[tokio::test]
async fn unmodified_cargo_fetches_a_crates_io_dependency_through_connect() {
    if !available("cargo", &["--version"]) || !available("python3", &["--version"]) {
        return;
    }
    let mut client = PackageClient::new();
    let output = checked(command("python3", &client.work).args(["-c", r#"
import hashlib, io, pathlib, tarfile
with tarfile.open('fixture.crate', 'w:gz') as archive:
    for path, body in [('Cargo.toml', '[package]\nname = "ggez-fixture"\nversion = "1.0.0"\nedition = "2021"\n'), ('src/lib.rs', 'pub const VALUE: &str = "through the package gateway";\n')]:
        body = body.encode()
        entry = tarfile.TarInfo('ggez-fixture-1.0.0/' + path)
        entry.size = len(body)
        entry.mode = 0o644
        archive.addfile(entry, io.BytesIO(body))
print(hashlib.sha256(pathlib.Path('fixture.crate').read_bytes()).hexdigest())
"#]));
    let checksum = String::from_utf8(output.stdout).unwrap();
    let origin = Origin::with_routes(HashMap::from([
        (("index.crates.io".into(), "/config.json".into()), (
            "application/json".into(), br#"{"dl":"https://static.crates.io/crates","api":"https://crates.io"}"#.to_vec(),
        )),
        (("index.crates.io".into(), "/gg/ez/ggez-fixture".into()), (
            "text/plain".into(), format!("{}\n", serde_json::json!({"name":"ggez-fixture","vers":"1.0.0","deps":[],"cksum":checksum.trim(),"features":{},"yanked":false})).into_bytes(),
        )),
        (("static.crates.io".into(), "/crates/ggez-fixture/1.0.0/download".into()), (
            "application/octet-stream".into(), fs::read(client.work.join("fixture.crate")).unwrap(),
        )),
    ])).await;
    fs::write(client.work.join("Cargo.toml"), "[package]\nname = \"gateway-client\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[dependencies]\nggez-fixture = \"=1.0.0\"\n").unwrap();
    fs::create_dir(client.work.join("src")).unwrap();
    fs::write(
        client.work.join("src/lib.rs"),
        "pub use ggez_fixture::VALUE;\n",
    )
    .unwrap();
    let proxy = client.proxy(&origin).await;
    let work = client.work.clone();
    tokio::task::spawn_blocking(move || {
        checked(
            command("cargo", &work)
                .arg("fetch")
                .env("CARGO_HOME", work.join("cargo"))
                .env("CARGO_REGISTRIES_CRATES_IO_PROTOCOL", "sparse")
                .env("CARGO_HTTP_PROXY", proxy)
                .env("CARGO_HTTP_CAINFO", work.join("ca.crt"))
                .env("CARGO_HTTP_TIMEOUT", "5")
                .env("CARGO_NET_RETRY", "0"),
        );
        let source = fs::read_dir(work.join("cargo/registry/src"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path()
            .join("ggez-fixture-1.0.0/src/lib.rs");
        assert!(
            fs::read_to_string(source)
                .unwrap()
                .contains("through the package gateway")
        );
        assert!(
            fs::read_to_string(work.join("Cargo.lock"))
                .unwrap()
                .contains("registry+https://github.com/rust-lang/crates.io-index")
        );
    })
    .await
    .unwrap();
    let requests = origin.requests.lock().unwrap();
    for (host, path) in [
        ("index.crates.io", "/config.json"),
        ("index.crates.io", "/gg/ez/ggez-fixture"),
        ("static.crates.io", "/crates/ggez-fixture/1.0.0/download"),
    ] {
        assert!(
            requests.iter().any(|r| r.0 == host && r.2 == path),
            "missing {host}{path}"
        );
    }
    assert!(
        requests
            .iter()
            .all(|r| r.1 == Method::GET && r.4.is_empty())
    );
}
