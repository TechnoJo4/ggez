#![forbid(unsafe_code)]

use guest_bridge::config::{Config, HELP};

fn main() -> std::process::ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() == 1 && (args[0] == "--help" || args[0] == "-h") {
        println!("{HELP}");
        return std::process::ExitCode::SUCCESS;
    }
    let result = Config::parse(args)
        .map_err(std::io::Error::other)
        .and_then(run);
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("guest-bridge: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(not(unix))]
fn run(_: Config) -> std::io::Result<()> {
    Err(std::io::Error::other("guest-bridge requires Unix sockets"))
}

#[cfg(unix)]
fn run(config: Config) -> std::io::Result<()> {
    use std::{
        fs,
        os::unix::fs::{MetadataExt, PermissionsExt},
        sync::Arc,
    };
    use tokio::{
        net::UnixListener,
        signal::unix::{SignalKind, signal},
        task::JoinSet,
    };

    // Never unlink a pre-existing pathname, including a stale socket.
    struct SocketFile {
        path: std::path::PathBuf,
        device: u64,
        inode: u64,
    }
    impl Drop for SocketFile {
        fn drop(&mut self) {
            if let Ok(meta) = fs::symlink_metadata(&self.path)
                && meta.dev() == self.device
                && meta.ino() == self.inode
            {
                let _ = fs::remove_file(&self.path);
            }
        }
    }

    tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(async {
        let listener = UnixListener::bind(&config.bridge_socket)?;
        let meta = fs::symlink_metadata(&config.bridge_socket)?;
        let _socket_file = SocketFile { path: config.bridge_socket.clone(), device: meta.dev(), inode: meta.ino() };
        fs::set_permissions(&config.bridge_socket, fs::Permissions::from_mode(config.socket_mode))?;
        let mut terminate = signal(SignalKind::terminate())?;
        let mut interrupt = signal(SignalKind::interrupt())?;
        let config = Arc::new(config);
        let mut sessions = JoinSet::new();
        loop {
            tokio::select! {
                _ = terminate.recv() => break,
                _ = interrupt.recv() => break,
                completed = sessions.join_next(), if !sessions.is_empty() => {
                    if let Some(Err(error)) = completed { eprintln!("guest-bridge: session task: {error}"); }
                }
                accepted = listener.accept() => {
                    let (client, _) = accepted?;
                    if sessions.len() >= config.max_connections { continue; }
                    let config = config.clone();
                    sessions.spawn(async move {
                        let result = async {
                            let guest = guest_bridge::connect(&config).await?;
                            guest_bridge::relay(client, guest, config.idle_timeout).await
                        }.await;
                        if let Err(error) = result { eprintln!("guest-bridge: session: {error}"); }
                    });
                }
            }
        }
        sessions.abort_all();
        while sessions.join_next().await.is_some() {}
        Ok(())
    })
}
