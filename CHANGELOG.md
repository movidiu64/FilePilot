# Changelog

All notable changes to FilePilot are listed here. Versions follow [Semantic Versioning](https://semver.org/):
a new major version may need you to review your rules, minor versions add features, patches fix bugs.

## [Unreleased]

## [1.1.0] - 2026-11-15

### Added
- **Extract archive** action: ZIP, RAR, 7z, TAR and more, with the extractor built into Windows. Extracts into a new folder named after the archive (a single top folder is not nested again), only shows the folder once it is complete, refuses entries that would escape it, and can be undone from the journal. The archive stays, so the next action can send it to the Recycle Bin.
- Template and setup-guide choice: *Extract downloaded archives*.
- An example rules file in English, `examples/downloads.orgrules`, to import and adapt.

## [1.0.0] - 2026-11-15

First stable release, and the first one under the name **FilePilot** (formerly *Organizator*). Licensed under the GNU GPL v3.

### Rules
- Watch any number of folders (with or without subfolders), including network and removable drives; an unavailable folder is picked up again when it comes back.
- If-then rules: nested "all of / any of" groups; conditions on name, extension, path, folder, size, dates and **file content** (text, DOCX, XLSX, ODT, PDF).
- Actions: move, copy, rename, change extension, send to the Recycle Bin, delete, run a command, **ask for confirmation**; variables in paths and names (dates, dictionaries, content).
- Each rule reads as a sentence above the editor; ready-made templates and a first-run guide.
- **Simulation mode**: a rule only logs what it would do; files are never touched.
- Hidden rules behind a password.

### Safety
- Files still being downloaded or in use are left alone until they are complete; nothing is executed or opened to check.
- Every operation goes to a write-ahead journal and can be restored (including from the Recycle Bin, keeping the original extension).
- Configurable immediate or delayed processing, pause from the tray icon, automatic rule backups.

### Duplicates
- Find files with exactly the same content, in the same folder or across folders; the oldest copy is always kept.
- A persistent fingerprint catalog: unchanged files are never read twice, so rescans and previews stay fast on large folders.
- A **Duplicates** page with groups, the space to free and a byte-by-byte check before anything goes to the Recycle Bin.

### Window
- Home page with status, recent actions, a 7-day chart and confirmations; journal with search, filters and restore; live preview of the files a rule matches, with progress and sorting.
- English and Romanian interface, light and dark theme.

### Installer
- Per-user NSIS installer (no admin rights) with Start menu entry, optional autostart and a clean uninstall that keeps your data unless you ask otherwise.
- Upgrading from *Organizator* is automatic: the old install is removed, autostart is carried over and the data folder moves from `%APPDATA%\Organizator` to `%APPDATA%\FilePilot`.
