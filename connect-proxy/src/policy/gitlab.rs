use super::{ForgeRead, UpstreamConfig, archive, reference};
use git_policy::clean_segment;
use hyper::Uri;
use reqwest::Url;

// The API project identifier is one encoded path segment; Nix escapes '/' and '.'.
fn project(value: &str) -> Option<String> {
    let mut decoded = String::new();
    let mut bytes = value.bytes();
    while let Some(b) = bytes.next() {
        if b == b'%' {
            let hi = (bytes.next()? as char).to_digit(16)?;
            let lo = (bytes.next()? as char).to_digit(16)?;
            decoded.push((hi * 16 + lo) as u8 as char);
        } else {
            decoded.push(b as char);
        }
    }
    (decoded.split('/').all(clean_segment)
        && (decoded.contains('/') || decoded.bytes().all(|b| b.is_ascii_digit())))
    .then_some(decoded)
}

fn selector(uri: &Uri, key: &str) -> Option<String> {
    let url = Url::parse(&format!("https://invalid.example/?{}", uri.query()?)).ok()?;
    let mut pairs = url.query_pairs();
    let (name, value) = pairs.next()?;
    (name == key && pairs.next().is_none() && reference(&value)).then(|| value.into_owned())
}

// Nix gitlab: and fetchFromGitLab both use these API routes, not Git.
pub(super) fn authorize(uri: &Uri) -> Option<ForgeRead> {
    let parts: Vec<_> = uri.path().strip_prefix('/')?.split('/').collect();
    match parts.as_slice() {
        ["api", "v4", "projects", slug, "repository", "commits"] => {
            project(slug)?;
            selector(uri, "ref_name")?;
            Some(ForgeRead::Lookup {
                accept: "application/json",
            })
        }
        [
            "api",
            "v4",
            "projects",
            slug,
            "repository",
            "archive.tar.gz",
        ] => archive(
            UpstreamConfig::Gitlab {},
            project(slug)?,
            &selector(uri, "sha")?,
        ),
        _ => None,
    }
}

pub(super) fn redirects_to(host: &str, next: UpstreamConfig, next_host: &str) -> bool {
    host == next_host && next == (UpstreamConfig::Gitlab {})
}
