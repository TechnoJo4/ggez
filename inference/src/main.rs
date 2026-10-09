#![forbid(unsafe_code)]

use inference::config::{Config, HELP};

fn main() -> std::process::ExitCode {
    host_runtime::entry("inference", HELP, Config::parse, run)
}

#[cfg(not(unix))]
fn run(_: Config) -> std::io::Result<()> {
    Err(std::io::Error::other("inference requires Unix sockets"))
}

#[cfg(unix)]
fn run(config: Config) -> std::io::Result<()> {
    use host_runtime::unix::{Listener, run};
    use std::sync::Arc;
    let listener = Listener {
        path: config.socket,
        mode: config.socket_mode,
        max_connections: config.max_connections,
    };
    let gateway = Arc::new(config.gateway);
    run("inference", listener, move |stream| {
        inference::server::serve(stream, gateway.clone(), config.deadline)
    })
}
