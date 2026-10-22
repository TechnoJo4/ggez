#![forbid(unsafe_code)]

mod client;
pub mod config;
mod policy;
mod protocols;
pub mod server;

fn content_type(headers: &hyper::HeaderMap, expected: &str) -> bool {
    headers
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .eq_ignore_ascii_case(expected)
        })
}

#[cfg(test)]
mod tests;
