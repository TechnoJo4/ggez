#![forbid(unsafe_code)]

use std::{ffi::OsString, io, process::ExitCode};

pub fn entry<C>(
    name: &str,
    help: &str,
    parse: impl FnOnce(Vec<OsString>) -> Result<C, String>,
    run: impl FnOnce(C) -> io::Result<()>,
) -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() == 1 && (args[0] == "--help" || args[0] == "-h") {
        println!("{help}");
        return ExitCode::SUCCESS;
    }
    match parse(args).map_err(io::Error::other).and_then(run) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{name}: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(unix)]
pub mod unix;

#[cfg(feature = "http")]
pub mod http;
