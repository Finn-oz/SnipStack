# Changelog

All notable changes to SnipStack will be documented in this file.

SnipStack is a hard fork of [EcoPaste](https://github.com/EcoPasteHub/EcoPaste)
(forked at upstream v1.1.0 line, 2026-08). For the history of the inherited
clipboard-manager codebase, see the upstream changelog.

## [Unreleased]

### Added

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
