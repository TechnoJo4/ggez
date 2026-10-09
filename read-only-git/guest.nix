deployment:
{ lib, ... }:
{
  imports = [ (import ../vsock-guest.nix deployment) ];
  programs.git = {
    enable = true;
    config.url = lib.mapAttrs' (name: upstream: lib.nameValuePair
      "http://127.0.0.1:${toString deployment.guestTcpPort}/${name}/"
      { insteadOf = upstream; }
    ) deployment.upstreams;
  };
}
