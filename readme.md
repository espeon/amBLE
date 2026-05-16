# amble

Control Amaran/Aputure lights over BLE Mesh Proxy.

```sh
amaran on [light]
amaran off [light]
amaran brightness <0-100> [light]
amaran cct <br> <kelvin> [gm] [light]
amaran hsi <br> <hue> <sat> [light]
amaran rgb <r> <g> <b> [brightness] [light]
amaran lights
amaran scan
amaran setup
amaran start          # launch daemon
amaran stop           # stop daemon
```

## setup

Run `amaran setup` to import keys from the Amaran Desktop database
(~/Library/Application Support/amaran Desktop) or enter them manually. Saves to
`lights.json`.

Also usable as a Rust library (`use amble::controller::MeshController`).

