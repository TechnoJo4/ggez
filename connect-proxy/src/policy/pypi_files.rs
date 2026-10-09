use super::ReadOperation;
use hyper::Uri;

// Warehouse stores artifacts under a BLAKE2b-256 digest split 2/2/60.
// The same filename with .metadata appended serves PEP 658/714 metadata.
pub(super) fn authorize(uri: &Uri) -> Option<ReadOperation> {
    if uri.query().is_some() {
        return None;
    }
    let parts: Vec<_> = uri.path().strip_prefix("/packages/")?.split('/').collect();
    let [first, second, digest, filename] = parts.as_slice() else {
        return None;
    };
    // pip quotes local-version '+' and epoch '!' in download links. Decode
    // only those characters; escaped separators, dots and '%' remain denied.
    let filename = filename
        .replace("%2B", "+")
        .replace("%2b", "+")
        .replace("%21", "!");
    let hex = |value: &str, len| {
        value.len() == len
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    };
    (hex(first, 2)
        && hex(second, 2)
        && hex(digest, 60)
        && !filename.is_empty()
        && filename != "."
        && filename != ".."
        && filename
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.+!".contains(&b)))
    .then_some(ReadOperation::PackageFile)
}
