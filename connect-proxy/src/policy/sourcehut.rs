use super::{ForgeRead, UpstreamConfig, archive};
use git_policy::clean_segment;
use hyper::Uri;

// Nix sourcehut: resolves refs with plain HEAD/info/refs reads, then an archive.
pub(super) fn authorize(uri: &Uri) -> Option<ForgeRead> {
    if uri.query().is_some() {
        return None;
    }
    let mut parts = uri.path().strip_prefix('/')?.splitn(3, '/');
    let owner = parts.next()?;
    let repo = parts.next()?;
    if !owner.starts_with('~') || !clean_segment(&owner[1..]) || !clean_segment(repo) {
        return None;
    }
    let path = parts.next()?;
    if matches!(path, "HEAD" | "info/refs") {
        return Some(ForgeRead::Lookup {
            accept: "text/plain",
        });
    }
    let rev = path.strip_prefix("archive/")?.strip_suffix(".tar.gz")?;
    archive(UpstreamConfig::Sourcehut {}, format!("{owner}/{repo}"), rev)
}

pub(super) fn redirects_to(host: &str, next: UpstreamConfig, next_host: &str) -> bool {
    host == next_host && next == (UpstreamConfig::Sourcehut {})
}
