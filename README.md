## cashewnix
A binary cache proxy that can fetch from other binary caches discovered on the local network.

Discovery works by first finding all interfaces that appear to be local networks and then
sending a message to their respective broadcast address. Other instances on the network
can then send a broadcast signed with the cache private key, which will be added
to the list of caches if the message can be authenticated with a known public key. All systems
must have their clocks somewhat synced, because all signed messages include a timestamp
to avoid replay attacks.

### Usage
TODO

### Prior work
- [peerix](https://github.com/cid-chan/peerix)
