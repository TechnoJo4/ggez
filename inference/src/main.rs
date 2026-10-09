#![forbid(unsafe_code)]

use inference::config::{Config, HELP};

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
            eprintln!("inference: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(not(unix))]
fn run(_: Config) -> std::io::Result<()> {
    Err(std::io::Error::other("inference requires Unix sockets"))
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
        let listener = UnixListener::bind(&config.socket)?;
        let meta = fs::symlink_metadata(&config.socket)?;
        let _socket_file = SocketFile { path: config.socket.clone(), device: meta.dev(), inode: meta.ino() };
        fs::set_permissions(&config.socket, fs::Permissions::from_mode(config.socket_mode))?;
        let mut terminate = signal(SignalKind::terminate())?;
        let mut interrupt = signal(SignalKind::interrupt())?;
        let gateway = Arc::new(config.gateway);
        let mut requests = JoinSet::new();
        loop {
            tokio::select! {
                _ = terminate.recv() => break,
                _ = interrupt.recv() => break,
                completed = requests.join_next(), if !requests.is_empty() => {
                    if let Some(Err(error)) = completed { eprintln!("inference: request task: {error}"); }
                }
                accepted = listener.accept() => {
                    let (stream, _) = accepted?;
                    if requests.len() >= config.max_connections { continue; }
                    let gateway = gateway.clone();
                    requests.spawn(async move {
                        if let Err(error) = inference::server::serve(stream, gateway, config.deadline).await {
                            eprintln!("inference: {error}");
                        }
                    });
                }
            }
        }
        requests.abort_all();
        while requests.join_next().await.is_some() {}
        Ok(())
    })
}
