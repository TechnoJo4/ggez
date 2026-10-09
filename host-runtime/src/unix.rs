use std::{
    fs,
    future::Future,
    io,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::PathBuf,
};
use tokio::{
    net::{UnixListener, UnixStream},
    signal::unix::{SignalKind, signal},
    task::JoinSet,
};

pub struct Listener {
    pub path: PathBuf,
    pub mode: u32,
    pub max_connections: usize,
}

struct SocketFile {
    path: PathBuf,
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

pub fn run<F, Fut>(name: &'static str, config: Listener, handle: F) -> io::Result<()>
where
    F: Fn(UnixStream) -> Fut,
    Fut: Future<Output = io::Result<()>> + Send + 'static,
{
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            // Binding refuses every pre-existing pathname, including stale sockets.
            let listener = UnixListener::bind(&config.path)?;
            let meta = fs::symlink_metadata(&config.path)?;
            let _socket_file = SocketFile {
                path: config.path,
                device: meta.dev(),
                inode: meta.ino(),
            };
            fs::set_permissions(&_socket_file.path, fs::Permissions::from_mode(config.mode))?;
            let mut terminate = signal(SignalKind::terminate())?;
            let mut interrupt = signal(SignalKind::interrupt())?;
            let mut sessions = JoinSet::new();
            loop {
                tokio::select! {
                    _ = terminate.recv() => break,
                    _ = interrupt.recv() => break,
                    completed = sessions.join_next(), if !sessions.is_empty() => {
                        if let Some(Err(error)) = completed {
                            eprintln!("{name}: session task: {error}");
                        }
                    }
                    accepted = listener.accept() => {
                        let (stream, _) = accepted?;
                        if sessions.len() >= config.max_connections { continue; }
                        let session = handle(stream);
                        sessions.spawn(async move {
                            if let Err(error) = session.await {
                                eprintln!("{name}: {error}");
                            }
                        });
                    }
                }
            }
            sessions.abort_all();
            while sessions.join_next().await.is_some() {}
            Ok(())
        })
}
