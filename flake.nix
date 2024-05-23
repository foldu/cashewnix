{
  description = "A thing.";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    crane = {
      url = "github:ipetkov/crane";
      inputs.nixpkgs.follows = "nixpkgs";
    };
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
        cashewnix =
          let
            cargoToml = builtins.fromTOML (builtins.readFile "${self}/Cargo.toml");
            version = cargoToml.package.version;
            pname = cargoToml.package.name;
            craneLib = crane.mkLib nixpkgs.legacyPackages.${system};
          in
          craneLib.buildPackage {
            inherit pname version;
            src =
              let
                data = path: _type: builtins.match ".*/data/.*" path != null;
              in
              pkgs.lib.cleanSourceWith {
                src = craneLib.path ./.;
                filter = path: type: (data path type) || (craneLib.filterCargoSources path type);
              };
          };
      in
      {
        packages = {
          inherit cashewnix;
          default = cashewnix;
        };
        devShell = pkgs.mkShell { nativeBuildInputs = [ ]; };
      }
    );
}
