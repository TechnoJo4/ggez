deployment:
{
  imports = [ (import ../vsock-guest.nix (deployment // { direction = "inbound"; })) ];
}
