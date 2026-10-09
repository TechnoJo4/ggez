use super::{ReadOperation, UpstreamConfig, archive};
use git_policy::clean_segment;
use hyper::Uri;

// fetchFromCodeberg/Gitea/Forgejo defaults to this archive, including tag refs.
pub(super) fn authorize(uri: &Uri) -> Option<ReadOperation> {
    if uri.query().is_some() {
        return None;
    }
    let mut parts = uri.path().strip_prefix('/')?.splitn(4, '/');
    let owner = parts.next()?;
    let repo = parts.next()?;
    if !clean_segment(owner) || !clean_segment(repo) || parts.next()? != "archive" {
        return None;
    }
    let rev = parts.next()?.strip_suffix(".tar.gz")?;
    archive(UpstreamConfig::Forgejo {}, format!("{owner}/{repo}"), rev)
}

pub(super) fn redirects_to(host: &str, next: UpstreamConfig, next_host: &str) -> bool {
    host == next_host && next == (UpstreamConfig::Forgejo {})
}
