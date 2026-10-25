use super::ReadOperation;
use hyper::Uri;

fn name(value: &str, max: usize) -> bool {
    (2..=max).contains(&value.len())
        && !value.starts_with('-')
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

// Deno validates SemVer; confine its spelling to one version object key.
fn version(value: &str) -> bool {
    value.as_bytes().first().is_some_and(u8::is_ascii_digit)
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".+-".contains(&b))
}

fn file_segment(value: &str) -> Option<()> {
    let mut decoded = Vec::with_capacity(value.len());
    let mut bytes = value.bytes();
    while let Some(byte) = bytes.next() {
        decoded.push(if byte == b'%' {
            let high = (bytes.next()? as char).to_digit(16)?;
            let low = (bytes.next()? as char).to_digit(16)?;
            (high * 16 + low) as u8
        } else {
            byte
        });
    }
    let decoded = std::str::from_utf8(&decoded).ok()?;
    // Reject separators, URL delimiters, nested escapes and nonportable names.
    (!decoded.is_empty()
        && decoded != "."
        && decoded != ".."
        && !decoded
            .chars()
            .any(|c| c.is_control() || "/\\%?#:*<>\"|".contains(c)))
    .then_some(())
}

pub(super) fn authorize(uri: &Uri) -> Option<ReadOperation> {
    if uri.query().is_some() {
        return None;
    }
    let parts: Vec<_> = uri.path().strip_prefix("/@")?.split('/').collect();
    let [scope, package, rest @ ..] = parts.as_slice() else {
        return None;
    };
    if !name(scope, 20) || !name(package, 58) {
        return None;
    }
    match rest {
        ["meta.json"] => Some(ReadOperation::Lookup {
            accept: "application/json",
        }),
        [file] if file.strip_suffix("_meta.json").is_some_and(version) => {
            Some(ReadOperation::Lookup {
                accept: "application/json",
            })
        }
        [ver, path @ ..]
            if version(ver)
                && !path.is_empty()
                && path.iter().all(|part| file_segment(part).is_some()) =>
        {
            Some(ReadOperation::PackageFile)
        }
        _ => None,
    }
}
