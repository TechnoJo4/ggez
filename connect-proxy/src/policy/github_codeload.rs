use super::{ReadOperation, UpstreamConfig, archive, revision, segments};
use hyper::Uri;

pub(super) fn authorize(uri: &Uri) -> Option<ReadOperation> {
    if uri.query().is_some() {
        return None;
    }
    let parts = segments(uri)?;
    let [owner, repo, "tar.gz" | "legacy.tar.gz", rev] = parts.as_slice() else {
        return None;
    };
    if !revision(rev) {
        return None;
    }
    archive(UpstreamConfig::Github {}, format!("{owner}/{repo}"), rev)
}
