use super::{ForgeRead, UpstreamConfig, archive, revision, segments};
use hyper::Uri;

pub(super) fn authorize(uri: &Uri) -> Option<ForgeRead> {
    if uri.query().is_some() {
        return None;
    }
    let parts = segments(uri)?;
    let [owner, repo, "archive", filename] = parts.as_slice() else {
        return None;
    };
    let rev = filename.strip_suffix(".tar.gz")?;
    if !revision(rev) {
        return None;
    }
    archive(UpstreamConfig::Github {}, format!("{owner}/{repo}"), rev)
}

// GitHub alone needs cross-host archive redirects (github -> codeload).
pub(super) fn redirects_to(next: UpstreamConfig) -> bool {
    matches!(
        next,
        UpstreamConfig::Github {}
            | UpstreamConfig::GithubApi {}
            | UpstreamConfig::GithubCodeload {}
    )
}
