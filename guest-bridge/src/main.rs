#![forbid(unsafe_code)]

use guest_bridge::config::{Config, HELP};

fn main() -> std::process::ExitCode {
    host_runtime::entry("guest-bridge", HELP, Config::parse, run)
}

#[cfg(not(unix))]
fn run(_: Config) -> std::io::Result<()> {
    Err(std::io::Error::other("guest-bridge requires Unix sockets"))
}

#[cfg(unix)]
fn run(config: Config) -> std::io::Result<()> {
    use host_runtime::unix::{Listener, run};
    use std::sync::Arc;
    let listener = Listener {
        path: config.bridge_socket.clone(),
        mode: config.socket_mode,
        max_connections: config.max_connections,
    };
    let config = Arc::new(config);
    run("guest-bridge", listener, move |client| {
        let config = config.clone();
        async move {
            let guest = guest_bridge::connect(&config).await?;
            guest_bridge::relay(client, guest, config.idle_timeout).await?;
            Ok(())
        }
    })
}
