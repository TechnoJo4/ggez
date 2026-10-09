# Host capability adapters. VSOCK CID 2 is the Firecracker host.
{ name, vsockPort, guestTcpPort ? vsockPort, direction ? "outbound", ... }:
{ lib, pkgs, ... }:
let
  addresses = if direction == "outbound" then
    "TCP4-LISTEN:${toString guestTcpPort},bind=127.0.0.1,reuseaddr,fork VSOCK-CONNECT:2:${toString vsockPort}"
  else
    "VSOCK-LISTEN:${toString vsockPort},reuseaddr,fork TCP4:127.0.0.1:${toString guestTcpPort}";
  capabilities = lib.optional
    (if direction == "outbound" then guestTcpPort < 1024 else vsockPort < 1024)
    "CAP_NET_BIND_SERVICE";
in {
  systemd.services."gateway-${name}" = {
    wantedBy = [ "multi-user.target" ];
    serviceConfig = {
      ExecStart = "${pkgs.socat}/bin/socat ${addresses}";
      Restart = "on-failure";
      DynamicUser = true;
      AmbientCapabilities = capabilities;
      CapabilityBoundingSet = capabilities;
      RestrictAddressFamilies = [ "AF_INET" "AF_VSOCK" ];
    };
  };
}
