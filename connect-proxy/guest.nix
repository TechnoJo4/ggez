deployment:
{ config, lib, ... }:
let
  proxy = "http://127.0.0.1:${toString deployment.guestTcpPort}";
  fetchEnvironment = {
    https_proxy = proxy;
    PIP_CERT = toString config.security.pki.caBundle;
    CARGO_HTTP_CAINFO = toString config.security.pki.caBundle;
    NIX_GIT_SSL_CAINFO = toString config.security.pki.caBundle;
    NIX_CURL_FLAGS = "--cacert ${config.security.pki.caBundle}";
  };
in {
  imports = [ (import ../vsock-guest.nix deployment) ];
  security.pki.certificateFiles = [ ./ca.crt ];
  environment.variables = fetchEnvironment;
  systemd.services.nix-daemon.environment = fetchEnvironment;
  # Source builders use nixpkgs' cacert rather than the guest's /etc trust bundle.
  nix.settings = {
    # Keep local aliases; the global registry is outside the allowed fetch routes.
    flake-registry = "";
    extra-sandbox-paths = [ config.security.pki.caBundle ];
  } // lib.optionalAttrs (deployment.nixSubstituters != []) {
    substituters = lib.mkForce deployment.nixSubstituters;
  };
}
