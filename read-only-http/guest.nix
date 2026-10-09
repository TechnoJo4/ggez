deployment:
{ lib, ... }:
{
  imports = [ (import ../vsock-guest.nix deployment) ];
  nix.settings.substituters = lib.mkForce [ "http://127.0.0.1:${toString deployment.guestTcpPort}" ];
}
