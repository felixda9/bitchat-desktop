# bitchat-desktop

Desktop client for [bitchat](https://github.com/permissionlesstech/bitchat) on Windows and Linux.

Work in progress. Location channels and private messages over Nostr work, and so does the Bluetooth mesh (#mesh) with phones nearby.

## Build

Needs Rust and Node.js. On Linux also `libwebkit2gtk-4.1-dev librsvg2-dev libssl-dev libdbus-1-dev`.

```
cd app
npm install
npx tauri dev
```

## Bluetooth on Linux

Needs BlueZ. Its battery plugin makes iPhones show a pairing request when they connect; bitchat doesn't need pairing, so turn the plugin off:

```
sudo mkdir -p /etc/systemd/system/bluetooth.service.d
printf '[Service]\nExecStart=\nExecStart=/usr/libexec/bluetooth/bluetoothd -P battery\n' | \
  sudo tee /etc/systemd/system/bluetooth.service.d/no-battery.conf
sudo systemctl daemon-reload && sudo systemctl restart bluetooth
```

(The path to `bluetoothd` can differ by distro; `/usr/lib/bluetooth/bluetoothd` on some.)

## License

Public domain ([Unlicense](LICENSE)).
