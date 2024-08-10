{
  inputs = {
    nixpkgs.url = "nixpkgs/nixos-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
    fenix.url = "github:nix-community/fenix/monthly";
  };

  outputs =
    inputs@{ self
    , flake-parts
    , fenix
    , nixpkgs
    , ...
    }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];

      perSystem =
        { pkgs, system, ... }:
        let
          _module.args.pkgs = import nixpkgs {
            inherit system;
            overlays = [ fenix.overlays.default ];
          };

          nativeBuildInputs = with pkgs; [
            pkg-config
            openssl
          ];

          buildInputs = with pkgs; [ ];

          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath buildInputs;

          devPackages = [
            fenix.packages.${system}.complete.toolchain
          ];
        in
        {
          devShells = {
            default = pkgs.mkShell {
              inherit LD_LIBRARY_PATH buildInputs nativeBuildInputs;
              packages = devPackages;
            };
          };
        };
    };
}
