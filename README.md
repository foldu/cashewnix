# cashewnix
A binary cache proxy that can fetch from other binary caches discovered on the local network.

Discovery works by first finding all interfaces that appear to be local networks and then
sending a message to their respective broadcast address. Other instances on the network
can then send a broadcast signed with the cache private key, which will be added
to the list of caches if the message can be authenticated with a known public key. All systems
must have their clocks somewhat synced, because all signed messages include a timestamp
to avoid replay attacks.

### Usage
This program is usable from the NixOS module included in this flake.
```nix
{ inputs, ... }:
{
  # ...
  imports = [ inputs.cashewnix.nixosModules.cashewnix ];
  # ...
}
```
You first need to generate a public/private keypair with:
```sh
nix-store --generate-binary-cache-key example.org ./cashewnix-private ./cashewnix-public
```
Where `example.org` is just an identifier put in front of the key. Put the private key
somewhere only readable by root. This key is used for both verifying the authenticity
of devices advertising on the local network and signing derivations. If you're lazy
you could re-use the same key on all devices in the network. The public key must
be added to the configuration of all devices, otherwise this node will be ignored because
it can't authenticate itself.

An example for a minimal configuration which shares the local store
with the network:
```nix
services.cashewnix = {
  enable = true;
  enableNixServe = true;
  privateKeyPath = "/var/secrets/cashewnix-private";
  settings.public_keys = [ 
    "cache.example.org-1:2asqQ4huy7+QY5Ll45TyVtFkmGYdwKzyUW/vODwJDk8="
  ];
};

services.nix-serve.openFirewall = true;
```

A configuration that can pull from other nodes but doesn't
share its own store:
```nix
services.cashewnix = {
  enable = true;
  settings.public_keys = [
    "cache.example.org-1:2asqQ4huy7+QY5Ll45TyVtFkmGYdwKzyUW/vODwJDk8="
  ];
};
```
If you're already hosting a binary cache on https:
```nix
services.cashewnix = {
  enable = true;
  privateKeyPath = "/var/secrets/cashewnix-private";
  settings = {
    public_keys = [
      "cache.example.org-1:2asqQ4huy7+QY5Ll45TyVtFkmGYdwKzyUW/vODwJDk8="
    ];
    local_binary_caches.local_cache = {
      advertise = "url";
      url = "https://my-binary-cache.tld";
    };
  };
};
```

For other options look inside [nix/module.nix].

### Prior work
- [peerix](https://github.com/cid-chan/peerix)
