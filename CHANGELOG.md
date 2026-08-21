# Changelog

All notable changes to SnipStack will be documented in this file.

SnipStack is a hard fork of [EcoPaste](https://github.com/EcoPasteHub/EcoPaste)
(forked at upstream v1.1.0 line, 2026-08). For the history of the inherited
clipboard-manager codebase, see the upstream changelog.

## [Unreleased]

## [0.1.3] - 2026-08-21

### Fixed

- Window geometry is no longer written to disk on the UI thread. Saving now
  happens on a dedicated background thread with atomic writes, so a slow
  disk, antivirus scan, or cloud-synced profile folder can no longer stall
  the app when a window hides — a suspected cause of the rare "tray icon
  disappears / app won't reopen" issue. State is still flushed synchronously
  on exit and before storage-location migration, so nothing is lost.
- Hang watchdog refinements: OS drag-and-drop is recognized as an expected
  pause (no false hang capture), while a genuine freeze during a drag is
  still captured after an escalated threshold; each distinct hang episode
  records at most one minidump.

## [0.1.2] - 2026-08-20

### Added

- Runtime diagnostics for troubleshooting rare "tray icon disappears /
  app won't reopen" reports: panics are logged with a backtrace, a
  watchdog records main-thread unresponsiveness together with GDI/USER
  handle counts and writes a minidump next to the logs, and a clean
  shutdown leaves an explicit marker in the log. Everything stays in the
  local log folder — SnipStack still sends no telemetry.

## [0.1.1] - 2026-08-14

### Added

- Download landing page (https://finn-oz.github.io/SnipStack/).

### Changed

- First release shipped through the auto-updater — this version verifies
  the update pipeline end-to-end for installed v0.1.0 users.

## [0.1.0] - 2026-08-14

First public release. Windows 11 x64, NSIS installer, unsigned (SmartScreen
will prompt on first run — see the README install notes).

### Added

- Snip-to-text: global hotkey (default `Alt+S`) or tray menu opens a
  per-monitor selection overlay; the selected region is recognized offline
  with PP-OCRv5 mobile, the text is copied to the clipboard and saved to
  history as a regular text entry (searchable, deduplicated, URL/email
  detection — identical to text you copy yourself). Line-break mode
  (keep / merge) and auto-copy are configurable in the Snip preferences tab.
- QR/barcode detection in the snip selection: codes are decoded directly
  (multiple codes joined by line) with OCR as the fallback; configurable.
- Downloadable OCR language packs (Korean, Latin-script, Russian/East
  Slavic, Thai, Arabic; 8-13 MB each) with in-app download, progress,
  SHA-256 validation, and a mirror source; the built-in model already covers
  Simplified/Traditional Chinese, English, and Japanese. Recognition falls
  back to the built-in model whenever the selected pack is missing, and the
  settings UI explains each pack's script coverage.
- Background OCR for copied images: images captured from the clipboard are
  recognized in the background and become full-text searchable; snips and
  already-indexed items are never re-processed or overwritten.
- Clipboard monitor honors the Windows exclusion format conventions
  (`ExcludeClipboardContentFromMonitorProcessing`, `Clipboard Viewer
  Ignore`, `CanIncludeInClipboardHistory=0`), so password managers like
  KeePass and 1Password are never recorded. Snip results follow the same
  secret-collection policy as copied text.
- Snip overlay focuses the monitor under the cursor, so Esc works
  immediately on multi-monitor setups; snip completion toast in the
  clipboard window (character count or error).
- New SnipStack app icon (selection brackets + card stack + text lines).
- Project forked from EcoPaste and rebranded to SnipStack
  (identifier `com.snipstack.app`, backup extension `.snipstackbak`,
  data dir `SnipStackData`).

### Changed

- Clicking "All" in the clipboard group bar now clears the category and
  custom-group filters instead of doing nothing.
- The C runtime is statically linked: the app runs on clean Windows
  installs without the VC++ Redistributable.
- Release pipeline builds a Windows x64 NSIS installer only and fetches
  OCR models during the build; auto-updater endpoints point to SnipStack
  GitHub Releases.

### Known issues

- "Run as administrator" (pasting into elevated windows) is hidden in this
  release: the elevation flow has known defects (onboarding window loss
  after elevated restart, autostart sync failure, single-instance guard
  not working across elevation levels). It will return once fixed.
