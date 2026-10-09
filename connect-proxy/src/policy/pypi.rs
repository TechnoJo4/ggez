use super::ReadOperation;
use hyper::Uri;

// pip requests normalized project pages with a trailing slash. HTML also
// carries wheel metadata links and remains compatible with older pip clients.
pub(super) fn authorize(uri: &Uri) -> Option<ReadOperation> {
    if uri.query().is_some() {
        return None;
    }
    let name = uri.path().strip_prefix("/simple/")?.strip_suffix('/')?;
    name.split('-')
        .all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
        .then_some(ReadOperation::Lookup {
            accept: "text/html",
        })
}
