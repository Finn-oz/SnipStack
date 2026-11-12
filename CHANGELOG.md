# Changelog

All notable changes to SnipStack will be documented in this file.

SnipStack is a hard fork of [EcoPaste](https://github.com/EcoPasteHub/EcoPaste)
(forked at upstream v1.1.0 line, 2026-08). For the history of the inherited
clipboard-manager codebase, see the upstream changelog.

## [Unreleased]

### Fixed

- Saving settings no longer blocks readers of the settings. Previously, on a slow
  disk (antivirus scans, cloud sync, failing hardware) the background update check
  writing settings could stall the UI thread along with it — another suspected cause
  of the intermittent "tray icon disappears / app cannot be opened again" issue.
- Snipping can no longer silently stop working until restart: Alt+F4 on the capture
  overlay now cancels the snip, and a capture overlay that never appears is cancelled
  automatically with a notice.
- History cleanup no longer deletes items you copied again recently: the retention
  period and item limit now go by the last time an item was copied or used rather
  than when it was first recorded.
- Copying the same content in quick succession no longer creates duplicate entries
  (duplicate image entries could also lose the image when one of them was deleted).
- Images, thumbnails and icons are now written atomically, so a full disk or a killed
  process can no longer leave a permanently broken image behind.
- When the settings file cannot be read, the original is kept as
  `settings.json.unreadable-*` before falling back to defaults instead of being
  silently overwritten; transient read failures are retried.
- The clipboard watcher is rebuilt automatically if it stops unexpectedly, instead of
  showing "listening" while no longer recording.
- Turning off launch at startup (including on first run) no longer fails with
  0x80070005.
- Update checks and downloads now time out instead of hanging forever on a stalled
  connection; installing an update and the tray "Relaunch" action now save window
  positions and leave a clean-shutdown marker in the log.
- The "Release notes" link in the update window pointed to the wrong address.
- With "Scroll to top on open" turned off, items copied while the window was hidden
  now show up when it opens.
- Dragging items out to other apps repeatedly no longer stacks up drop-forwarding
  layers.
- Changing the data directory no longer reports failure when the switch succeeded
  but the old directory could not be fully removed.
- An extremely large retention value no longer disables the cleanup task.
- When a custom data directory is temporarily unavailable (USB drive unplugged,
  network or late-mounted volume not ready yet), the default directory is used for
  that run only instead of permanently resetting the configuration; the custom
  directory is picked up again on the next start once it is back. Previously,
  re-selecting the original directory in that state replaced its history with
  nearly empty data.
- Changing the data directory no longer deletes SnipStack data already present at the
  target; it is kept next to it in a `*.replaced-*` folder, and a copy that fails
  midway leaves the target as it was.
- An overwrite import that fails midway now restores the database, images and
  settings to their state before the import, instead of leaving damaged history or
  a database connection that only recovers after a restart.
- Importing a backup exported by a newer SnipStack version is now refused with a
  notice before any existing data is touched; previously it could leave the app
  unable to start.
- After importing a backup, shortcuts, launch-at-startup and tray settings take
  effect immediately, and state that belongs to the original computer (onboarding
  progress, update-check records) is no longer carried over.
- While a delete / clear confirmation is open, Enter and Esc now act on the
  confirmation instead of pasting the selected item or hiding the panel.
- Checking for updates again after an update was downloaded no longer discards the
  downloaded installer.
- The "Snip → Save to History" description now matches the behavior (only the
  recognized text is saved).

## [0.1.5] - 2026-08-22

### Fixed

- The app icon shown inside the app is now SnipStack's own: the clipboard
  window header, preference sidebar, onboarding screens, update window, and
  the fallback icon for entries without a source app were all still showing
  the upstream EcoPaste logo, because the frontend icon assets were missed
  when the app was rebranded (window, tray, and installer icons were already
  correct).

## [0.1.4] - 2026-08-22

### Fixed

- The installer now ships the Microsoft Visual C++ runtime DLLs next to
  `SnipStack.exe`. On a fresh Windows install without the VC++
  Redistributable, every previous version failed to start with
  "MSVCP140_1.dll was not found": the OCR engine's statically linked ONNX
  Runtime still depends on the C++ standard library, which the 0.1.0
  static-CRT change did not cover (the 0.1.0 note claiming the app installs
  without the VC++ Redistributable was therefore inaccurate). No separate
  runtime installation is needed anymore.

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
