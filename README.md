# SnipStack

**Snip any text on screen. Keep everything you ever copied.**

SnipStack is a Windows 11 tool that combines a TextSniper-style screen-capture
OCR with a full clipboard history manager:

- **Snip → Text**: press a global hotkey, draw a rectangle anywhere on screen
  (video, PDF, remote desktop, protected UI…), and the recognized text lands in
  your clipboard. Fully offline OCR (PP-OCRv5), Simplified Chinese + English at
  launch, QR/barcode decoding included.
- **Everything becomes searchable history**: every snip (image + recognized
  text) and everything you copy (text + images) goes into a local, full-text
  searchable history — find any screenshot again by the words inside it.
- **Local-first & private**: no cloud, no account. Honors the clipboard
  formats password managers use to exclude sensitive content; configurable
  history limits and retention.

> **Status: early development.** The clipboard-history foundation works; the
> screen-capture OCR pipeline is under construction. Windows 11 only.

[简体中文说明](./README.zh-CN.md)

## Development

Prerequisites: Windows 11, [Rust](https://rustup.rs/) (see
`rust-toolchain.toml`), Node.js + [pnpm](https://pnpm.io/).

```bash
pnpm install
pnpm tauri dev
```

## Roadmap

- **M1** — Snip-to-text MVP: hotkey → per-monitor selection overlay → offline
  PP-OCRv5 OCR → clipboard + history.
- **M2** — QR/barcode decoding, line-break modes, background OCR for copied
  images, clipboard privacy conventions, history TTL.
- **M3** — Mixed-DPI multi-monitor polish, downloadable language packs, NSIS
  installer, v0.1 release.

## Credits & License

SnipStack is a hard fork of
[EcoPaste](https://github.com/EcoPasteHub/EcoPaste) by
[ayangweb](https://github.com/ayangweb) — the clipboard watcher, FTS5 storage,
window management, and settings infrastructure originate there. Thank you!

Licensed under the [Apache License 2.0](./LICENSE). See [NOTICE](./NOTICE).
