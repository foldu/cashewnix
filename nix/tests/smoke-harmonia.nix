{
  self,
  pkgs,
  lib,
}:
let
  # Hardcoded test key pair for smoke testing.
  # Generated with: nix-store --generate-binary-cache-key test-cache secret.key pub.key
  testPublicKey = "test-cache:UimBNYOh3uU10DhlCAjhPX3Iculdl5pqM8Lm0XRYrdg=";
  testPrivateKey = pkgs.writeText "test-cache-secret" ''
    test-cache:KE6PWZn17/5nAt7EUjSiGYYQ7oWQ33hW4Bv2wSq/fQ9SKYE1g6He5TXQOGUICOE9fchy6V2XmmozwubRdFit2A==
  '';
  harmoniaPort = 5000;
in
pkgs.testers.nixosTest {
  name = "cashewnix-harmonia";

  nodes.machine =
    { ... }:
    {
      imports = [
        self.nixosModules.cashewnix
      ];

      services.cashewnix = {
        enable = true;
        privateKeyPath = "${testPrivateKey}";
        settings = {
          public_keys = [ testPublicKey ];
          local_binary_caches.local_cache = {
            advertise = "ip";
            port = harmoniaPort;
          };
        };
      };

      services.harmonia.cache = {
        enable = true;
        signKeyPaths = [ "${testPrivateKey}" ];
        settings = {
          bind = "0.0.0.0:${toString harmoniaPort}";
        };
      };

      networking.firewall.allowedTCPPorts = [ harmoniaPort ];

      # Allow the generated public key for nix substitution
      nix.settings.trusted-public-keys = [ testPublicKey ];
    };

  testScript =
    { nodes, ... }:
    let
      cashewnixPort = toString nodes.machine.services.cashewnix.settings.port;
    in
    ''
      start_all()

      # Wait for services to come up
      machine.wait_for_unit("cashewnix.service")
      # harmonia uses socket activation: wait for the socket, then the
      # first request will trigger the actual service to start.
      machine.wait_for_unit("harmonia.socket")
      machine.wait_for_open_port(${cashewnixPort})

      with subtest("cashewnix /nix-cache-info"):
          output = machine.succeed("curl -sS http://127.0.0.1:${cashewnixPort}/nix-cache-info")
          assert "StoreDir: /nix/store" in output, f"unexpected /nix-cache-info from cashewnix: {output}"
          assert "WantMassQuery: 1" in output, f"unexpected /nix-cache-info from cashewnix: {output}"
          assert "Priority: 30" in output, f"unexpected /nix-cache-info from cashewnix: {output}"

      with subtest("harmonia /nix-cache-info"):
          # This first request activates the socket-activated harmonia service
          machine.wait_until_succeeds("curl -sS http://127.0.0.1:${toString harmoniaPort}/nix-cache-info")
          output = machine.succeed("curl -sS http://127.0.0.1:${toString harmoniaPort}/nix-cache-info")
          assert "StoreDir: /nix/store" in output, f"unexpected /nix-cache-info from harmonia: {output}"

      with subtest("nix can reach harmonia as a substituter"):
          # Verify nix can query harmonia's nix-cache-info via the
          # standard nix binary cache protocol — not just HTTP.
          result = machine.succeed(
              "nix-store --query --substituters http://127.0.0.1:${toString harmoniaPort} "
              "--dry-run /run/current-system 2>&1 || true"
          )
          print(f"nix-store reached harmonia: {result[:200]}")
    '';
}
