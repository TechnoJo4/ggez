use super::ReadOperation;
use hyper::Uri;

pub(super) fn crate_name(name: &str) -> bool {
    name.len() <= 64
        && name.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
}

// Sparse index files have lowercase names and exactly Cargo's shard prefix.
pub(super) fn authorize(uri: &Uri) -> Option<ReadOperation> {
    if uri.query().is_some() {
        return None;
    }
    let path = uri.path().strip_prefix('/')?;
    if path != "config.json" {
        let name = path.rsplit('/').next()?;
        if !crate_name(name) || name.bytes().any(|b| b.is_ascii_uppercase()) {
            return None;
        }
        let expected = match name.len() {
            1 => format!("1/{name}"),
            2 => format!("2/{name}"),
            3 => format!("3/{}/{name}", &name[..1]),
            _ => format!("{}/{}/{name}", &name[..2], &name[2..4]),
        };
        if path != expected {
            return None;
        }
    }
    Some(ReadOperation::Lookup { accept: "*/*" })
}
