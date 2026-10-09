use super::{ReadOperation, cargo_index::crate_name};
use hyper::Uri;

// Cargo validates SemVer; this keeps its spelling within one crate object key.
fn version(version: &str) -> bool {
    version.as_bytes().first().is_some_and(u8::is_ascii_digit)
        && version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".+-".contains(&b))
}

pub(super) fn authorize(uri: &Uri) -> Option<ReadOperation> {
    if uri.query().is_some() {
        return None;
    }
    let parts: Vec<_> = uri.path().strip_prefix('/')?.split('/').collect();
    let (name, ver) = match parts.as_slice() {
        ["crates", name, ver, "download"] => (*name, *ver),
        ["crates", name, file] => (
            *name,
            file.strip_suffix(".crate")?
                .strip_prefix(name)?
                .strip_prefix('-')?,
        ),
        _ => return None,
    };
    (crate_name(name) && version(ver)).then_some(ReadOperation::PackageFile)
}
