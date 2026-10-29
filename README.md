# bitchat-desktop

Desktop client for [bitchat](https://github.com/permissionlesstech/bitchat) on Windows and Linux.

Work in progress. Location channels and private messages over Nostr work. Bluetooth mesh comes later.

## Build

Needs Rust and Node.js. On Linux also `libwebkit2gtk-4.1-dev librsvg2-dev libssl-dev libdbus-1-dev`.

```
cd app
npm install
npx tauri dev
```

## License

Public domain ([Unlicense](LICENSE)).
