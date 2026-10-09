#![cfg(unix)]

use std::{
    fs,
    os::unix::fs::{FileTypeExt, PermissionsExt},
    path::PathBuf,
    process::{Child, Command},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
    time::timeout,
};

struct Fixture {
    root: PathBuf,
    process: Option<Child>,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "ggez-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        Self {
            root,
            process: None,
        }
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_guest-bridge"));
        cmd.args([
            "--guest-listener-vsock-port",
            "5003",
            "--host-firecracker-vsock-uds",
        ])
        .arg(self.root.join("firecracker.sock"))
        .arg("--host-bridge-listener-uds")
        .arg(self.root.join("bridge.sock"))
        .args(["--max-connections", "1", "--handshake-timeout-seconds", "1"]);
        cmd
    }

    async fn start(&mut self) -> UnixStream {
        self.process = Some(self.command().spawn().unwrap());
        timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(stream) = UnixStream::connect(self.root.join("bridge.sock")).await {
                    return stream;
                }
                assert!(
                    self.process.as_mut().unwrap().try_wait().unwrap().is_none(),
                    "bridge exited at startup"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(process) = &mut self.process {
            let _ = process.kill();
            let _ = process.wait();
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn real_uds_round_trip_and_connection_limit() {
    let mut fixture = Fixture::new();
    let firecracker = UnixListener::bind(fixture.root.join("firecracker.sock")).unwrap();
    let mut client = fixture.start().await;
    let (mut guest, _) = timeout(Duration::from_secs(5), firecracker.accept())
        .await
        .unwrap()
        .unwrap();
    let mut command = [0; 13];
    guest.read_exact(&mut command).await.unwrap();
    assert_eq!(&command, b"CONNECT 5003\n");
    guest.write_all(b"OK 1073741824\nhello\0").await.unwrap();
    let mut greeting = [0; 6];
    client.read_exact(&mut greeting).await.unwrap();
    assert_eq!(&greeting, b"hello\0");
    let meta = fs::metadata(fixture.root.join("bridge.sock")).unwrap();
    assert!(meta.file_type().is_socket());
    assert_eq!(meta.permissions().mode() & 0o777, 0o660);

    let mut excess = UnixStream::connect(fixture.root.join("bridge.sock"))
        .await
        .unwrap();
    let result = timeout(Duration::from_secs(2), excess.read_u8())
        .await
        .unwrap();
    assert!(result.is_err(), "excess connection was admitted");

    client.write_all(b"request").await.unwrap();
    client.shutdown().await.unwrap();
    let mut received = Vec::new();
    guest.read_to_end(&mut received).await.unwrap();
    assert_eq!(received, b"request");
    guest.write_all(b"reply").await.unwrap();
    guest.shutdown().await.unwrap();
    let mut reply = Vec::new();
    client.read_to_end(&mut reply).await.unwrap();
    assert_eq!(reply, b"reply");

    let process = fixture.process.as_mut().unwrap();
    assert!(
        Command::new("kill")
            .args(["-TERM", &process.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    timeout(Duration::from_secs(5), async {
        loop {
            if let Some(status) = process.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(!fixture.root.join("bridge.sock").exists());
}

#[test]
fn existing_path_is_never_removed() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("bridge.sock"), b"do not unlink").unwrap();
    assert!(!fixture.command().status().unwrap().success());
    assert_eq!(
        fs::read(fixture.root.join("bridge.sock")).unwrap(),
        b"do not unlink"
    );
}
