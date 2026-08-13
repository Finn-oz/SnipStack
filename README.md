# SnipStack

**Snip any text on screen. Keep everything you ever copied.**

SnipStack is a free, open-source Windows 11 tool that combines
TextSniper-style screen-capture OCR with a full clipboard history manager:

- **Snip → Text**: press a global hotkey (default `Alt+S`), draw a rectangle
  anywhere on screen (video, PDF, remote desktop, protected UI…), and the
  recognized text lands in your clipboard and history. Fully offline OCR
  (PP-OCRv5) covering Simplified/Traditional Chinese, English and Japanese
  out of the box, with downloadable packs for Korean, Latin-script languages,
  Russian, Thai and Arabic. QR/barcode decoding included.
- **Everything becomes searchable history**: every snip and everything you
  copy (text + images) goes into a local, full-text searchable history —
  copied images are OCR'd in the background so you can find a screenshot
  again by the words inside it.
- **Local-first & private**: no cloud, no account, no telemetry. Honors the
  clipboard formats password managers use to exclude sensitive content;
  configurable history limits and retention.

[简体中文说明](./README.zh-CN.md)

## Download & Install

**[Download the latest release](https://github.com/Finn-oz/SnipStack/releases/latest)**
— get `SnipStack_x.y.z_x64-setup.exe` (Windows 11, x64).

1. Run the installer. It installs per-user — no administrator rights needed.
2. Windows SmartScreen may warn about an unknown publisher (the installer is
   not code-signed yet). If the file came from the official releases page,
   click **More info → Run anyway**. You can verify the download first:

   ```powershell
   Get-FileHash .\SnipStack_x.y.z_x64-setup.exe -Algorithm SHA256
   ```

   and compare against the SHA-256 published in the release notes.
3. Launch SnipStack from the Start Menu. It lives in the system tray
   (check the `^` overflow area of the taskbar).

First steps: press `Alt+C` to open your clipboard history, `Alt+S` to snip
text from the screen. Both hotkeys are configurable in Preferences.

## Privacy & data handling

- Clipboard history, snip results and settings are stored **locally only**,
  under `%LOCALAPPDATA%\com.snipstack.app`. Nothing leaves your device.
- OCR runs fully on-device with bundled models. The only network requests
  the app makes are the GitHub update check and optional OCR language-pack
  downloads.
- No account, no cloud sync, no telemetry or analytics.
- Content copied from password managers that mark it with the standard
  exclusion formats (`ExcludeClipboardContentFromMonitorProcessing`,
  `Clipboard Viewer Ignore`, `CanIncludeInClipboardHistory=0`) is never
  recorded. Text that looks like a secret (API keys, tokens) is skipped
  unless you opt in. These conventions are best-effort, not a security
  boundary — treat your history database like any other local file.
- You can cap history size/retention, and delete individual items or
  everything at any time from the app.

## Known limitations

- Windows 11 x64 only.
- The installer is unsigned for now, so SmartScreen shows a warning on
  first run (see above).
- "Run as administrator" (for pasting into elevated apps) is disabled in
  this release while its flow is being fixed.
- Selecting a downloaded OCR language pack recognizes that script (plus
  basic Latin letters) only — switch back to the built-in model for Chinese.

## Development

Prerequisites: Windows 11, [Rust](https://rustup.rs/) (see
`rust-toolchain.toml`), Node.js + [pnpm](https://pnpm.io/).

```bash
pnpm install
pnpm fetch:ocr-models
pnpm build:icon
pnpm tauri dev
```

`fetch:ocr-models` downloads the PP-OCRv5 mobile models (~21 MB) into
`src-tauri/resources/ocr/` — they are not committed to git. Manual test
checklist: [docs/testing-win11.md](./docs/testing-win11.md). Contribution
guide: [CONTRIBUTING.md](./CONTRIBUTING.md).

## Releasing (maintainers)

Tag `v*` triggers the release workflow (Windows x64 NSIS installer,
attached to a draft GitHub Release built from both changelogs). Both
changelog files must contain a section matching the tagged version.
Requires the `TAURI_SIGNING_PRIVATE_KEY` / `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`
repository secrets for updater artifact signing.

## Credits & License

SnipStack is an independent hard fork of
[EcoPaste](https://github.com/EcoPasteHub/EcoPaste) by
[ayangweb](https://github.com/ayangweb) — the clipboard watcher, FTS5 storage,
window management, and settings infrastructure originate there. Thank you!
SnipStack is not affiliated with or endorsed by the EcoPaste project.

OCR is powered by [PP-OCRv5](https://github.com/PaddlePaddle/PaddleOCR)
models via [oar-ocr](https://github.com/GreatV/oar-ocr). See
[THIRD-PARTY-NOTICES.md](./THIRD-PARTY-NOTICES.md).

Licensed under the [Apache License 2.0](./LICENSE). See [NOTICE](./NOTICE).
