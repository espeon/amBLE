# amble

control Amaran/Aputure lights over BLE Mesh. ported (ish) from [Wes Bos' version](https://github.com/wesbos/amaran-BLE-control)


## commands
```sh
amble                # REPL mode
amble on [light]
amble off [light]
amble brightness <0-100> [light]
amble cct <br> <kelvin> [gm] [light]
amble hsi <br> <hue> <sat> [light]
amble lights
amble scan
amble setup
amble start          # launch daemon
amble stop           # stop daemon
```

## quick setup

run `amble setup` to import keys from the Amaran Desktop app, or enter them manually.

also usable as a Rust library if you
want to embed BLE mesh control in your own tooling.
