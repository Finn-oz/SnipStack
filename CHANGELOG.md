# Changelog

All notable changes to SnipStack will be documented in this file.

SnipStack is a hard fork of [EcoPaste](https://github.com/EcoPasteHub/EcoPaste)
(forked at upstream v1.1.0 line, 2026-08). For the history of the inherited
clipboard-manager codebase, see the upstream changelog.

## [Unreleased]

### Added

- Downloadable OCR language packs (Korean, Latin-script, Russian/East
  Slavic, Thai, Arabic; 8-13 MB each) with in-app download, progress,
  size validation, and a mirror source; the built-in model already covers
  Simplified/Traditional Chinese, English, and Japanese. Recognition
  falls back to the built-in model whenever the selected pack is missing.
- Snip overlay now focuses the monitor under the cursor, so Esc works
  immediately on multi-monitor setups.
- QR/barcode detection in the snip selection: codes are decoded directly
  (multiple codes joined by line) with OCR as the fallback; configurable.
- Background OCR for copied images: images captured from the clipboard are
  recognized in the background and become full-text searchable; snips and
  already-indexed items are never re-processed or overwritten.
- Clipboard monitor now honors the Windows exclusion format conventions
  (`ExcludeClipboardContentFromMonitorProcessing`, `Clipboard Viewer
  Ignore`, `CanIncludeInClipboardHistory=0`), so password managers like
  KeePass and 1Password are never recorded.
- Snip completion toast in the clipboard window (character count or error).
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

### Changed

- Release workflow now builds a Windows x64 NSIS installer only and
  fetches OCR models during the build.

### Planned

- Windows 11 end-to-end validation and the mixed-DPI test matrix
  (docs/testing-win11.md).
- Updater signing key and first public release.
