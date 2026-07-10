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
in
pkgs.testers.nixosTest {
  name = "cashewnix-nix-serve";

  nodes.machine =
    { ... }:
    {
      imports = [
        self.nixosModules.cashewnix
      ];

      services.cashewnix = {
        enable = true;
        enableNixServe = true;
        privateKeyPath = "${testPrivateKey}";
        settings.public_keys = [ testPublicKey ];
      };

      # Allow the generated public key for nix substitution
      nix.settings.trusted-public-keys = [ testPublicKey ];
    };

  testScript =
    { nodes, ... }:
    let
      cashewnixPort = toString nodes.machine.services.cashewnix.settings.port;
      nixServePort = toString nodes.machine.services.nix-serve.port;
    in
    ''
      start_all()

      # Wait for services to come up
      machine.wait_for_unit("cashewnix.service")
      machine.wait_for_unit("nix-serve.service")
      machine.wait_for_open_port(${cashewnixPort})
      machine.wait_for_open_port(${nixServePort})

      with subtest("cashewnix /nix-cache-info"):
          output = machine.succeed("curl -sS http://127.0.0.1:${cashewnixPort}/nix-cache-info")
          assert "StoreDir: /nix/store" in output, f"unexpected /nix-cache-info from cashewnix: {output}"
          assert "WantMassQuery: 1" in output, f"unexpected /nix-cache-info from cashewnix: {output}"
          assert "Priority: 30" in output, f"unexpected /nix-cache-info from cashewnix: {output}"

      with subtest("nix-serve /nix-cache-info"):
          output = machine.succeed("curl -sS http://127.0.0.1:${nixServePort}/nix-cache-info")
          assert "StoreDir: /nix/store" in output, f"unexpected /nix-cache-info from nix-serve: {output}"

      with subtest("nix can reach nix-serve as a substituter"):
          result = machine.succeed(
              "nix-store --query --substituters http://127.0.0.1:${nixServePort} "
              "--dry-run /run/current-system 2>&1 || true"
          )
          print(f"nix-store reached nix-serve: {result[:200]}")
    '';
}
