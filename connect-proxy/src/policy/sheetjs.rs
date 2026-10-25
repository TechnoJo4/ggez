use super::ReadOperation;
use hyper::Uri;

// SheetJS distributes current xlsx npm tarballs outside the npm registry.
pub(super) fn authorize(uri: &Uri) -> Option<ReadOperation> {
    if uri.query().is_some() {
        return None;
    }
    let (release, file) = uri.path().strip_prefix('/')?.split_once('/')?;
    let version = release.strip_prefix("xlsx-")?;
    let file_version = file.strip_prefix("xlsx-")?.strip_suffix(".tgz")?;
    (version == file_version
        && version.split('.').count() == 3
        && version
            .split('.')
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit())))
    .then_some(ReadOperation::PackageFile)
}
