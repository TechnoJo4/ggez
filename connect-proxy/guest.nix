deployment:
{ ... }:
let
  proxy = "http://127.0.0.1:${toString deployment.guestTcpPort}";
in {
  imports = [ (import ../vsock-guest.nix deployment) ];
  security.pki.certificateFiles = [ ./ca.crt ];
  environment.variables.https_proxy = proxy;
  systemd.services.nix-daemon.environment.https_proxy = proxy;
}
