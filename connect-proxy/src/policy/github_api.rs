use super::{ReadOperation, UpstreamConfig, archive, revision, segments};
use hyper::Uri;

pub(super) fn authorize(uri: &Uri) -> Option<ReadOperation> {
    if uri.query().is_some() {
        return None;
    }
    let parts = segments(uri)?;
    match parts.as_slice() {
        ["repos", _, _, "commits", _, ..] => Some(ReadOperation::Lookup {
            accept: "application/vnd.github+json",
        }),
        ["repos", owner, repo, "tarball", rev] if revision(rev) => {
            archive(UpstreamConfig::Github {}, format!("{owner}/{repo}"), rev)
        }
        _ => None,
    }
}
