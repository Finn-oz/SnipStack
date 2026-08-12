# Changelog

All notable changes to SnipStack will be documented in this file.

SnipStack is a hard fork of [EcoPaste](https://github.com/EcoPasteHub/EcoPaste)
(forked at upstream v1.1.0 line, 2026-08). For the history of the inherited
clipboard-manager codebase, see the upstream changelog.

## [Unreleased]

### Added

- Snip-to-text: global hotkey (default `Alt+S`) or tray menu opens a
  per-monitor selection overlay; the selected region is recognized offline
  with PP-OCRv5 mobile (Chinese + English), the text is copied to the
  clipboard, and the snip image is saved to history with its recognized
  text as searchable content. Line-break mode (keep / merge) and auto-copy
  are configurable in a new Snip preferences tab.
- Project forked from EcoPaste and rebranded to SnipStack
  (identifier `com.snipstack.app`, backup extension `.snipstackbak`,
  data dir `SnipStackData`).

### Changed

- Auto-updater endpoints now point to SnipStack GitHub Releases
  (`latest.json` convention); updater artifacts disabled until first release.

### Planned

- Screen-capture OCR: global hotkey → region selection → offline OCR
  (PP-OCRv5 mobile, Chinese + English) → clipboard + history.
- QR/barcode decoding in captured regions.
- Line-break handling modes (keep / merge) for OCR results.
- Background OCR of copied images with FTS-searchable text.
- Clipboard privacy: honor monitor-exclusion clipboard formats; history
  size/TTL limits.
