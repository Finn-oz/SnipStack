# Security Policy

SnipStack handles clipboard contents and on-screen text, which can include
sensitive data. Security reports are taken seriously.

## Supported versions

Only the latest release is supported. SnipStack is maintained by a solo
developer; fixes ship as new releases rather than backports.

## Reporting a vulnerability

Please use **GitHub private vulnerability reporting**:
[Report a vulnerability](https://github.com/Finn-oz/SnipStack/security/advisories/new).

Do not open public issues for security problems. You should receive an
initial response within a week. Please include reproduction steps, the app
version, and your Windows version.

## Scope notes

- Clipboard history is stored locally and unencrypted (standard SQLite);
  local-machine access is outside the threat model, as with other clipboard
  managers.
- The clipboard exclusion-format conventions and secret detection are
  best-effort filters, not a security boundary.
