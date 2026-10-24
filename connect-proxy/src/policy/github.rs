use super::{ReadOperation, UpstreamConfig, archive, reference, revision, segments};
use hyper::Uri;

pub(super) fn authorize(uri: &Uri) -> Option<ReadOperation> {
    if uri.query().is_some() {
        return None;
    }
    let parts = segments(uri)?;
    match parts.as_slice() {
        [_, _, "releases.atom"] => Some(ReadOperation::Lookup {
            accept: "application/atom+xml",
        }),
        [_, _, "commits", branch @ ..] => {
            let branch = branch.join("/");
            reference(branch.strip_suffix(".atom")?).then_some(ReadOperation::Lookup {
                accept: "application/atom+xml",
            })
        }
        [owner, repo, "archive", filename] => {
            let rev = filename.strip_suffix(".tar.gz")?;
            if !revision(rev) {
                return None;
            }
            archive(UpstreamConfig::Github {}, format!("{owner}/{repo}"), rev)
        }
        _ => None,
    }
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
