use std::{
    fs,
    future::Future,
    io,
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    time::Duration,
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
        if let Ok(_lock) = lock_parent(&self.path)
            && let Ok(meta) = fs::symlink_metadata(&self.path)
            && meta.dev() == self.device
            && meta.ino() == self.inode
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn lock_parent(path: &Path) -> io::Result<fs::File> {
    // Serialize pathname changes across gateways without leaving lockfiles.
    let directory = fs::File::open(path.parent().unwrap_or(Path::new(".")))?;
    directory.lock()?;
    Ok(directory)
}

async fn bind(path: &Path) -> io::Result<UnixListener> {
    let _lock = lock_parent(path)?;
    let error = match UnixListener::bind(path) {
        Ok(listener) => return Ok(listener),
        Err(error) if error.kind() == io::ErrorKind::AddrInUse => error,
        Err(error) => return Err(error),
    };
    let meta = fs::symlink_metadata(path)?;
    // Only ECONNREFUSED proves there is no listener; permissions and a full
    // backlog must not cause us to take over an active socket.
    if !meta.file_type().is_socket()
        || !matches!(
            tokio::time::timeout(Duration::from_secs(1), UnixStream::connect(path)).await,
            Ok(Err(error)) if error.kind() == io::ErrorKind::ConnectionRefused
        )
    {
        return Err(error);
    }
    let current = fs::symlink_metadata(path)?;
    if current.dev() != meta.dev() || current.ino() != meta.ino() {
        return Err(error);
    }
    fs::remove_file(path)?;
    UnixListener::bind(path)
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
            let listener = bind(&config.path).await?;
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
