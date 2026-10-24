# connect proxy

from the repo root, create the CA below; `ca.crt` is exported to the guest and `ca.key` stays on the host (ignored by git); set `GATEWAY_UID/GID` to match compose if overriding the defaults, including through `.env`.

```sh
umask 077
openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 \
  -out connect-proxy/ca.key
openssl req -new -x509 -sha256 -days 3650 \
  -key connect-proxy/ca.key -out connect-proxy/ca.crt \
  -subj '/CN=ggez CONNECT CA' \
  -addext 'basicConstraints=critical,CA:TRUE,pathlen:0' \
  -addext 'keyUsage=critical,keyCertSign,cRLSign' \
  -addext 'subjectKeyIdentifier=hash'
chmod 644 connect-proxy/ca.crt
sudo chown "${GATEWAY_UID:-65532}:${GATEWAY_GID:-65532}" connect-proxy/ca.key
```

## development guidelines

this gateway is a "last-resort" option to support existing tools; avoid stuffing capabilities into it. capabilities should be strictly read-only.

each policy/capability supported by this gateway MUST be listed below with the reason behind its inclusion.

specifically:

- `git`: nixpkgs' Git builders bypass system Git config (including our `insteadOf` rewrites).
- `github`:
  - nix's `github:` fetches and nixpkgs' archive fetches bypass Git.
  - `nix-update` uses `/:owner/:repo/releases.atom` Atom feeds.
- `github-api`: Nix resolves GitHub refs through the commit API.
- `github-codeload`: GitHub redirects archive downloads here.
- `forgejo`: nixpkgs' Codeberg/Gitea/Forgejo fetchers download archives without Git.
- `gitlab`: Nix's `gitlab:` and nixpkgs' GitLab fetchers use the HTTP API.
- `sourcehut`: Nix's `sourcehut:` and nixpkgs' SourceHut fetchers use HTTP refs/archives.
- `nix-cache`: Nix cache transport consolidated here to avoid a separate gateway.
- `pypi`, `pypi-files`: pip follows package index links to a separate artifact host; CONNECT preserves those URLs.
- `cargo-index`, `cargo-files`: Cargo's sparse index specifies a separate artifact host; CONNECT preserves its download URLs.
