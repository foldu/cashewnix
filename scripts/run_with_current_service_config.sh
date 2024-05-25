#!/bin/sh
export CONFIG_PATH="$(systemctl cat cashewnix | grep CONFIG_PATH | sed -E 's/.*"CONFIG_PATH=(.+)"/\1/')"
private_key_file="$(systemctl cat cashewnix | grep LoadCredential | sed -E 's|LoadCredential=.+:(.+)|\1|')"
echo Reading private key
export NIX_STORE_PRIVATE_KEY="$(sudo cat "$private_key_file")"
cargo r --release
