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
  settings =
    if cfg.settings.local_binary_caches.local_cache == null && cfg.enableNixServe then
      (lib.recursiveUpdate cfg.settings {
        local_binary_caches.local_cache = {
          advertise = "ip";
          port = config.services.nix-serve.port;
        };
      })
    else
      cfg.settings;
  configFile = json.generate "cashewnix.json" settings;
in
{
  options.services.cashewnix = {
    enable = lib.mkEnableOption "Enables cashewnix local binary cache";

    openFirewall = lib.mkOption {
      type = types.bool;
      default = true;
      description = lib.mdDoc ''
        Open needed UDP port 49745 on all interfaces.
      '';
    };

    privateKeyPath = lib.mkOption {
      type = types.nullOr types.str;
      example = "/var/secrets/cashewnix-private";
      default = null;
      description = lib.mdDoc ''
        Path to the private key. This is shared with nix-serve.
      '';
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
          public_keys = lib.mkOption {
            type = lib.types.listOf lib.types.str;
            example = [ "cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=" ];
            default = [ ];
            description = lib.mdDoc ''
              List of accepted public keys.
            '';
          };
          priority = lib.mkOption {
            type = lib.types.ints.u8;
            default = 30;
            description = lib.mdDoc ''
              Priority advertised in /nix-cache-info, highest priority is
              0 while everything above is lower. Default cache.nixos.org priority is 40.
            '';
          };
          local_binary_caches = lib.mkOption {
            type = types.submodule {
              options = {
                discovery_refresh_time = lib.mkOption {
                  type = types.str;
                  default = "60s";
                  description = lib.mdDoc ''
                    Frequency with which to update the local binary caches.
                  '';
                };
                local_cache = lib.mkOption {
                  type = types.nullOr types.anything;
                  example = {
                    advertise = "url";
                    url = "https://my-localcache.tld";
                  };
                  default = null;
                  description = lib.mdDoc ''
                    Configuration of the advertised local cache. Can be either
                    `ip` with an associated port or `url` with an url.
                  '';
                };
              };
            };
            default = { };
          };
          binary_caches = lib.mkOption {
            type = lib.types.listOf lib.types.anything;
            default = [ ];
            example = [
              {
                url = "https://cache.nixos.org";
                priority = "40";
              }
            ];
            description = lib.mdDoc ''
              Additional list of binary caches cashewnix can use.
            '';
          };
          priority_config = lib.mkOption {
            type = lib.types.anything;
            default = {
              "0" = {
                timeout = "500ms";
              };
            };
            description = lib.mdDoc ''
              Configuration of priority buckets. Priority 0 is
              the priority for local caches.
            '';
          };
        };
      };
    };

    enableNixServe = lib.mkOption {
      type = types.bool;
      default = false;
      description = lib.mdDoc ''
        Enable nix-serve to serve packages to the network.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = !cfg.enableNixServe || (cfg.privateKeyPath != null);
        message = "privateKeyPath must be defined if nix-serve is enabled";
      }
    ];

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
          Restart = "always";
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

    services.nix-serve = lib.mkIf cfg.enableNixServe {
      enable = true;
      secretKeyFile = cfg.privateKeyPath;
      # original nix-serve is way too slow and will cause errors
      package = pkgs.nix-serve-ng;
    };
  };
}
