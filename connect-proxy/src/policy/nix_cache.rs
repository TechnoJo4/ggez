use super::ReadOperation;
use hyper::Uri;

// The binary cache protocol reads metadata and NARs; no other cache API.
pub(super) fn authorize(uri: &Uri) -> Option<ReadOperation> {
    let clean = |part: &str| {
        !part.is_empty()
            && part != "."
            && part != ".."
            && part
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    };
    let path = uri.path().strip_prefix('/')?;
    if path != "nix-cache-info"
        && !path.strip_suffix(".narinfo").is_some_and(clean)
        && !path.strip_prefix("nar/").is_some_and(clean)
    {
        return None;
    }
    if let Some(query) = uri.query()
        && !query.strip_prefix("sha256=").is_some_and(clean)
    {
        return None;
    }
    Some(ReadOperation::Cache)
}
