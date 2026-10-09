{
  description = "Generate the guest's gateway modules";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { nixpkgs, ... }: {
    packages = nixpkgs.lib.genAttrs [ "x86_64-linux" "aarch64-linux" ] (system:
      let pkgs = nixpkgs.legacyPackages.${system};
      in {
        default = pkgs.writeShellApplication {
          name = "generate-nix";
          runtimeInputs = [ pkgs.python3 pkgs.docker-compose ];
          text = ''
            exec python3 ${./generate-nix} --checkout "$PWD" "$@"
          '';
        };
      });
  };
}
