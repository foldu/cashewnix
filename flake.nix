{
  description = "A thing.";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
    }:
    {
      nixosModules.cashewnix = import ./nix/module.nix { inherit self; };
    }
    // flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
        lib = pkgs.lib;

        commonArgs = {
          pname = "cashewnix";
          version = "0.1.0"; # keep in sync with Cargo.toml

          # The whole git tree; gitignored paths are excluded automatically.
          src = ./.;

          # Unlike Cargo.toml, Cargo.lock is plain TOML 1.0, so no normalizer
          # and no crane needed. buildRustPackage never parses Cargo.toml.
          cargoLock.lockFile = ./Cargo.lock;

          strictDeps = true;

          buildInputs = lib.optionals pkgs.stdenv.hostPlatform.isDarwin [
            pkgs.libiconv
          ];
        };

        cashewnix = pkgs.rustPlatform.buildRustPackage (commonArgs // { doCheck = false; });
      in
      {
        checks = {
          nextest = pkgs.rustPlatform.buildRustPackage (commonArgs // { useNextest = true; });
        }
        // lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
          smoke-test-nix-serve = import ./nix/tests/smoke-nix-serve.nix {
            inherit self pkgs lib;
          };
          smoke-test-harmonia = import ./nix/tests/smoke-harmonia.nix {
            inherit self pkgs lib;
          };
        };
        packages = {
          inherit cashewnix;
          default = cashewnix;
        };
        devShell = pkgs.mkShell { nativeBuildInputs = [ ]; };
      }
    );
}
