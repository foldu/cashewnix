{ self }:
{
  config,
  pkgs,
  lib,
  ...
}:
let
  cfg = config.services.cashewnix;
  types = lib.types;
  json = pkgs.formats.json { };
  configFile = json.generate "cashewnix.json" cfg.settings;
in
# TODO: make module easier to use
# merge various fields into settings
# TODO: document module properly
{
  options.services.cashewnix = {
    enable = lib.mkEnableOption "Enables cashewnix local binary cache";

    openFirewall = lib.mkOption {
      type = types.bool;
      default = true;
      description = ''
        Open needed UDP port 49745 on all interfaces.
      '';
    };

    privateKeyPath = lib.mkOption {
      type = types.nullOr types.str;
      default = null;
    };

    settings = lib.mkOption {
      type = lib.types.submodule {
        freeformType = json.type;

        options = {
          port = lib.mkOption {
            type = lib.types.port;
            default = 9543;
            description = lib.mdDoc ''
              Port to bind to.
            '';
          };
        };
      };
    };

    # TODO: use a submodule declaration

    enableNixServe = lib.mkOption {
      type = types.bool;
      default = true;
      description = ''
        Enable nix-serve to serve packages to the network.
      '';
    };

    openNixServeFirewall = lib.mkOption {
      type = types.bool;
      default = false;
      description = lib.mdDoc ''
        Open nix-serve port.
      '';
    };

    nixServePort = lib.mkOption {
      type = types.port;
      default = 4539;
      description = lib.mdDoc ''
        Port nix-serve listens on.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    networking.firewall.allowedUDPPorts = lib.mkIf cfg.openFirewall [ 49745 ];

    systemd.services.cashewnix =
      let
        pkg = self.packages.${pkgs.system}.cashewnix;
      in
      {
        wantedBy = [
          "multi-user.target"
          "network.target"
        ];

        environment = {
          CONFIG_PATH = configFile;
        };

        serviceConfig = {
          Restart = "on-failure";
          DynamicUser = "yes";
          RuntimeDirectory = "cashewnix";
          StateDirectory = "cashewnix";
          CacheDirectory = "cashewnix";
          LoadCredential = lib.optionalString (
            cfg.privateKeyPath != null
          ) "private_key:${cfg.privateKeyPath}";
        };

        script = ''
          ${lib.optionalString (cfg.privateKeyPath != null) ''
            export NIX_STORE_PRIVATE_KEY=$(cat $CREDENTIALS_DIRECTORY/private_key)
          ''}
          exec ${pkg}/bin/cashewnix
        '';
      };

    nix = {
      settings = {
        substituters = [ "http://127.0.0.1:${toString cfg.settings.port}" ];
        trusted-public-keys = config.services.cashewnix.settings.public_keys;
      };
    };

    services.nix-serve = {
      enable = cfg.enableNixServe;
      port = cfg.nixServePort;
      secretKeyFile = cfg.privateKeyPath;
      openFirewall = cfg.openNixServeFirewall;
    };
  };
}
