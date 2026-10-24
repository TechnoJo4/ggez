use super::ReadOperation;
use hyper::Uri;

fn name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 214
        && !name.starts_with(['.', '_'])
        && name != "-"
        && name != "~"
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.~".contains(&b))
}

// Only the slash separating a scope from its package may be escaped.
fn package<'a>(parts: &'a [&'a str]) -> Option<(&'a str, &'a [&'a str])> {
    let first = *parts.first()?;
    if let Some(scoped) = first.strip_prefix('@') {
        let (scope, package, rest) = if let Some((scope, package)) = scoped
            .split_once("%2f")
            .or_else(|| scoped.split_once("%2F"))
        {
            (scope, package, &parts[1..])
        } else {
            (scoped, *parts.get(1)?, &parts[2..])
        };
        (name(scope) && name(package) && scope.len() + package.len() + 2 <= 214)
            .then_some((package, rest))
    } else {
        name(first).then_some((first, &parts[1..]))
    }
}

// The client validates SemVer; these checks confine it to one object key.
fn version(version: &str) -> bool {
    version.as_bytes().first().is_some_and(u8::is_ascii_digit)
        && version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".+-".contains(&b))
}

pub(super) fn authorize(uri: &Uri) -> Option<ReadOperation> {
    if uri.query().is_some() {
        return None;
    }
    let parts: Vec<_> = uri.path().strip_prefix('/')?.split('/').collect();
    // JSR's npm tarballs include a transpiler revision, independent of SemVer.
    if let ["~", revision, "@jsr", package, file] = parts.as_slice() {
        let (scope, package) = package.split_once("__")?;
        return (!revision.is_empty()
            && revision.bytes().all(|b| b.is_ascii_digit())
            && name(scope)
            && name(package)
            && file.strip_suffix(".tgz").is_some_and(version))
        .then_some(ReadOperation::PackageFile);
    }
    let (name, rest) = package(&parts)?;
    match rest {
        [] => Some(ReadOperation::Lookup {
            accept: "application/json",
        }),
        [selector]
            if !selector.is_empty()
                && !selector.starts_with(['.', '_', '-'])
                && selector
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.~+".contains(&b)) =>
        {
            Some(ReadOperation::Lookup {
                accept: "application/json",
            })
        }
        ["-", file]
            if file
                .strip_suffix(".tgz")?
                .strip_prefix(name)?
                .strip_prefix('-')
                .is_some_and(version) =>
        {
            Some(ReadOperation::PackageFile)
        }
        _ => None,
    }
}
