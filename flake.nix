{
  description = "A thing.";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    crane.url = "github:ipetkov/crane";
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      crane,
    }:
    {
      nixosModules.cashewnix = import ./nix/module.nix { inherit self; };
    }
    // flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
        lib = pkgs.lib;
        craneLib = crane.mkLib nixpkgs.legacyPackages.${system};
        src =
          let
            isData = path: _type: builtins.match ".*/data/.*" path != null;
            isDeny = path: _type: builtins.match ".*deny\.toml" != null;
          in
          pkgs.lib.cleanSourceWith {
            src = craneLib.path ./.;
            filter =
              path: type: (isData path type) || (craneLib.filterCargoSources path type) || (isDeny path type);
          };
        commonArgs =
          let
            cargoToml = builtins.fromTOML (builtins.readFile "${self}/Cargo.toml");
            version = cargoToml.package.version;
            pname = cargoToml.package.name;
          in
          {
            inherit src version pname;
            strictDeps = true;

            buildInputs =
              [
                # Add additional build inputs here
              ]
              ++ lib.optionals pkgs.stdenv.isDarwin [
                # Additional darwin specific inputs can be set here
                pkgs.libiconv
              ];

            # Additional environment variables can be set directly
            # MY_CUSTOM_VAR = "some value";
          };
        cargoArtifacts = craneLib.buildDepsOnly commonArgs;
        cashewnix = craneLib.buildPackage commonArgs // {
          doCheck = false;
          inherit cargoArtifacts;
        };
      in
      {
        checks = {
          deny = craneLib.cargoDeny { inherit src; };
          nextest = craneLib.cargoNextest (
            commonArgs
            // {
              inherit cargoArtifacts;
              partitions = 1;
              partitionType = "count";
            }
          );
        };
        packages = {
          inherit cashewnix;
          default = cashewnix;
        };
        devShell = pkgs.mkShell { nativeBuildInputs = [ ]; };
      }
    );
}
