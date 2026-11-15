# FilePilot guide

FilePilot watches folders and applies *if–then* rules to the files that appear or change in them. This guide covers everything the [README](../README.md) only mentions.

- [How it works](#how-it-works)
- [Rules](#rules)
- [Conditions](#conditions)
- [Actions](#actions)
- [Archives](#archives)
- [Variables](#variables) (and [your variables](#your-variables))
- [Search](#search)
- [Duplicates](#duplicates)
- [Confirmations](#confirmations)
- [Simulation](#simulation)
- [Journal and restore](#journal-and-restore)
- [When a file is processed](#when-a-file-is-processed)
- [Safety](#safety)
- [Hidden rules](#hidden-rules)
- [Settings](#settings)
- [Data files](#data-files)
- [Command line](#command-line)
- [Installer](#installer)
- [Architecture](#architecture)
- [Releasing](#releasing)

## How it works

FilePilot is two programs that share one data folder, `%APPDATA%\FilePilot`:

- **`filepilot.exe`**, the daemon, runs all the time with an icon in the notification area (tray). It listens for folder changes through `ReadDirectoryChangesW` and sleeps when there are none. With *Start with Windows* on (Settings), it starts at sign-in through the user's `Run` registry key.
- **`filepilot-ui.exe`**, the window, is started on demand from the tray icon or the Start menu. It edits the rules, shows the journal and restores operations. Closing it does not stop the daemon.

The tray icon menu has **Open**, **Pause for 1 hour**, **Pause until resumed**, **Resume** and **Quit**. Its tooltip shows today's operations and errors, or until when FilePilot is paused. The number of pending confirmations appears on the icon too.

When the daemon starts, when a rule changes or after a pause, it scans the watched folders once. After that it only looks at files that are created or changed. Long scans show their progress on the Home page and in the status label at the top of the window.

## Rules

A rule has:

1. **Watched folders**, each with or without its subfolders. Network shares and removable drives work too: a folder that is unavailable is marked as such on the Home page and picked up again automatically when it comes back.
2. **If**: the conditions a file must meet.
3. **Then**: the actions to run, in order.

The sentence above the editor reads the rule back in plain words, and the *Matching files* list below it shows, for every file that matches now, exactly what the rule would do (the *Result* column), whether the file was already processed, and its size and dates. Click a column header to sort, as in Explorer. For large folders the list shows its progress and can be stopped with partial results.

**Order matters.** Rules run from top to bottom. Once a rule moves or renames a file, the rules below it do not see that file, unless the rule has *After a move, later rules still work on the file* ticked. A file sent to the Recycle Bin or deleted is never passed on, and neither is a file that failed or waits for confirmation. A copy or a command leaves the file where it is, so the next rules still see it. Put specific rules (invoices, duplicates) above the general sorting rule.

A file is processed the first time FilePilot sees it and again every time it changes. **Mark as pending** (on a file in the list, or *All pending* for the whole rule) makes the rule process it again, even if it was restored from the journal. Renaming a rule does not make it reprocess anything: rules have a permanent internal id.

**New rule** offers templates: sort Downloads by type, archive files older than 30 days, PDF invoices by year and month, clean up temporary files, duplicate downloads, backup copy. On first start, when there are no rules yet, a short guide asks which folder to watch and what to do with it, then shows how many files match right now. The rules it creates start in simulation mode. It can be run again from Home → Rules → *Setup guide*.

Rules can be exported to and imported from `.orgrules` files (plain JSON). Each rule can also be edited as JSON from its menu. [`examples/downloads.orgrules`](../examples/downloads.orgrules) has three ready rules (extract archives, PDF invoices, sort by type) to import: they arrive stopped and in simulation, with `C:\Users\you\Downloads` as the folder to change.

## Conditions

The first condition is **All of the following** (*and*), **Any of the following** (*or*) or **All files**. Groups can be nested up to 8 levels deep to build any logical expression.

| Condition | Looks at | Example |
|---|---|---|
| File name | the name without folder and extension | `c:\docs\letter.docx` → `letter` |
| File extension | the extension | `docx` |
| File path | the full path | `c:\docs\letter.docx` |
| Folder path | the folder that holds the file | `c:\docs` |
| File content | the text inside the file | see below |
| File size | larger than / smaller than, in B, KB, MB, GB (1 KB = 1024 B) | |
| Date created, date modified | before / after a date, older than N days, within the last N days | |
| Duplicate | the file has an older identical copy | see [Duplicates](#duplicates) |
| Variable | one of your variables finds a value in the file, or finds nothing | see [Your variables](#your-variables) |

Text conditions offer *is*, *is not*, *contains*, *does not contain*, *starts with*, *ends with* and *is empty*, and ignore letter case. For a range (size between two values) put two conditions in an *All of the following* group.

To skip a subfolder while watching with subfolders, add *folder path does not contain "temp"* (skips the folder and everything below it) or *folder path is not "c:\docs\temp"* (skips that exact folder only).

**File content** reads plain text (UTF-8, UTF-16, Windows-1250), DOCX, XLSX, ODT and PDF. A scanned PDF has text only after it was run through OCR. Documents over 64 MB, encrypted PDFs and binary files (images, archives) have no content. Content conditions are checked last, after the cheap ones, and the text is read once per file for all rules. The result of each content condition is remembered for each version of a file, so unchanged documents are not read again after a restart or a rule change.

Conditions with *older than N days* are re-checked hourly, so files that grow old enough are picked up without being changed.

## Actions

| Action | What it does |
|---|---|
| Move file | Moves the file to a folder, created if missing. *Keep subfolder structure* recreates the subfolders of the watched folder at the destination. *Remove folder if empty* deletes the folder the file came from if it is now empty, and empty parents up to (not including) the watched folder. |
| Copy file | Copies the file. *Following actions work on the copy* makes the next actions apply to the copy instead of the original. |
| Rename file | Changes the name, not the extension; usually with variables, e.g. `{name}_{created:YYYY-MM-DD}`. |
| Change extension | Changes only the extension. The content is not converted. |
| Send to Recycle Bin | Recoverable from the Recycle Bin and from the journal. Use this while testing a rule. |
| Delete permanently | Cannot be undone. |
| Run a program | Starts a program with arguments, without a shell, so arguments cannot inject commands. |
| Run a command (cmd) | Runs a line through `cmd /C` (`copy`, `echo`, `>>`, `&&` work). Variable values are quoted, so a file name containing `&` or `>` cannot start another command. A non-zero exit code is logged as an error and is not retried automatically. |
| Ask for confirmation | Stops the rule until you approve or reject; see [Confirmations](#confirmations). |
| Extract archive | Extracts the archive into a new folder; see [Archives](#archives). |

**If the file exists** at the destination:

- *Rename the new file* (default): `invoice.pdf` becomes `invoice_2.pdf`, `invoice_3.pdf`…; nothing is lost.
- *Skip*: the file stays where it is.
- *Overwrite the existing file*: the existing file goes to the Recycle Bin first, so this can be undone too.
- *Identical copy → Recycle Bin*: if the file at the destination has exactly the same content, the new one goes to the Recycle Bin; otherwise it is renamed.

**Destinations.** A relative destination (`PDF\{modified:YYYY}`) is relative to the watched folder. `{folder}` or `{path}` at the start of a destination means next to the file (`{folder}\PDF`); in the middle it reproduces the file's folder as a branch (`D:\Backup\{folder}` → `D:\Backup\C\Users\…`). Names Windows refuses (`CON`, `NUL`, `COM1`…, a trailing dot or space, more than 255 characters) are corrected before a move or rename.

> [!WARNING]
> A *Run a program* action with `{path}` as the program would start the very file the rule found. The editor warns about it: use a program that only reads the file (an antivirus, an archiver), or restrict the rule to safe extensions.

## Archives

**Extract archive** unpacks ZIP, RAR, 7z, TAR (also `.tar.gz`, `.tgz`, `.tar.xz`, `.tar.bz2`), CAB and ISO files with the `tar` built into Windows 10 and 11 (libarchive). Nothing extra needs to be installed.

- The destination is a **new folder**, by default `{folder}\{name}`: next to the archive, named after it. If the archive holds a single top folder, its contents go straight into the destination (no `photos\photos\…`).
- If the folder already exists, the archive goes into a new one (`photos_2`), or, with *Do not extract*, nothing happens. Files are never mixed into an existing folder.
- Files are extracted into `<folder>.filepilot-part` first and the folder gets its final name only when everything succeeded, so a half-extracted folder never appears, not even after a power cut. Rules ignore files inside such a folder.
- The archive itself stays where it is, so the next action can send it to the Recycle Bin. If extraction fails (damaged archive, password, not enough space), the next actions do not run and the archive is kept.
- **Restore** in the journal sends the extracted folder to the Recycle Bin. If files in it changed after extraction, FilePilot asks first. The rule does not extract that archive again unless it changes or you mark it as pending.
- Entries with absolute paths or `..` are refused: an archive cannot write outside its folder.

Limits: password-protected archives cannot be extracted, nor RAR archives split into parts (`.part1.rar`, `.r00`); the later parts are skipped without an error. The formats that work depend on the `tar` in your Windows version: older Windows 10 builds may not read archives made by recent WinRAR versions (RAR5). If an archive fails, the journal shows the message from `tar`.

## Variables

Variables can be used in destinations, names, arguments and commands. The **{ } Variable** button next to each field lists them with a preview.

| Variable | Value |
|---|---|
| `{name}` | file name without extension |
| `{ext}` | extension |
| `{file}` | name with extension |
| `{folder}` | the folder that holds the file |
| `{path}` | full path |
| `{size}` | size in bytes |
| `{created:FORMAT}`, `{modified:FORMAT}`, `{now:FORMAT}` | date created, date modified, today |
| `{var:Name}` | one of your variables (below) |

Date formats use one of two sets of tokens (the picker inserts them for you, in the interface language):

| | Year | Two-digit year | Month | Month name | Day | Hour | Minute | Second |
|---|---|---|---|---|---|---|---|---|
| English | `YYYY` | `YY` | `MM` | `MMMM` | `DD` | `HH` | `mm` | `ss` |
| Romanian | `AAAA` | `AA` | `LL` | `LLLL` | `ZZ` | `HH` | `MM` | `SS` |

A format uses one set or the other: `MM` is the month in an English format and the minute in a Romanian one (a format with `YY`, `DD`, `MMMM`, `mm` or `ss`, or just `MM`, is English). Month names follow the interface language. Any other character is kept, so `{modified:YYYY-MM-DD}` gives `2026-11-15` and `{created:YYYY}\{created:MM}` gives the subfolders `2026\11`. To write a literal brace, double it: `{{` and `}}`.

### Your variables

The **Variables** tab holds variables you define once and use in any rule, as `{var:Name}` in a destination, name or command, or in a **Variable** condition (*finds a value* / *finds nothing*).

**New variable** starts from a ready-made example (due date of an invoice, first date in the document, invoice number, supplier, total due, year from the file name, file type) that comes with a sample document, so the preview shows the result right away; or from an empty variable of a chosen type. In a rule, the **{ } Variable** menu and the **Variable** condition also offer **+ New variable…**: the variable is put in the rule and opened in the Variables tab, with a button back to the rule.

The editor has numbered steps, with the preview beside them: **what it looks for** (the type and its settings), **where it looks** (the file's text, its name, extension, path or folder), **which match counts** and **if nothing is found**. The types:

| Type | Value | Example |
|---|---|---|
| Dictionary | the value paired with the first key found | extension `pdf, doc, docx` → `Documents`, `jpg, png` → `Images` |
| Date in the text | a date found in the text, written in your format | `15.11.2026`, `2026-11-15`, `15 noiembrie 2026`, `November 15, 2026` → `2026-11` |
| Word from a list | the word or phrase found, as written in the list (whole words only) | `Enel`, `Digi`, `Orange Romania` |
| Pattern | the text that matches a simple pattern | `INV-####` → `INV-0042` |
| Regular expression | the first group of the expression, or the whole match | `Nr\.?\s*(\d+)` on `Nr. 4521` → `4521` |

The types other than *Dictionary* also have:

- **Which match**: the first, the second… or the last value found.
- **Written right before / after** (optional): only values next to that text count, with any spaces in between. With `Due date:` before it, a date variable takes the due date, not the issue date.
- **Preview**: paste a document's text or load a file to see every value found; the one used is highlighted. For a variable that looks in the name or path, type a file name or choose a file.

Pattern signs (the editor has a button for each and says the pattern in words, e.g. *“INV-” + 4 digits*): `#` a digit, `@` a letter, `?` any character, `%` a number (`1234`, `1.234,56`), `*` any text on the same line (as little as possible, so put something after it), a space for any whitespace; `\` before a sign takes it literally (`\#`). Regular expressions support classes, groups, alternatives, `^ $ \b` and repetitions, ignore letter case and run in time proportional to the text, so a rule never hangs on one; backreferences and lookarounds are not supported. Day-first dates are assumed (`05/03/2026` is 5 March) unless only month-first makes a valid date (`11/25/2026`).

If a variable finds nothing and has no fallback value, the actions that use it do not run for that file: the file stays where it is, and the journal records the step as failed with the reason (*the variable “Due” found no value in this file*). Such a file is not tried again until it changes. To leave these files alone quietly, put the condition *Variable … finds a value* in the rule. For variables that search the file's text, the text is read once per file for all rules, and the result of a **Variable** condition is remembered for each version of the file, like content conditions. Variables can be imported, exported and edited as JSON.

Example: invoices filed by supplier and month. *Supplier* is a word-from-a-list variable (`Enel`, `Digi`, `E.ON`, default `Other`) and *Due* a date variable with format `YYYY-MM` and `Due date:` written right before it; the rule's condition is *Variable Due finds a value* and its action *Move to* `D:\Invoices\{var:Supplier}\{var:Due}`.

## Search

The **Search** page finds files in the folders your rules watch, like a small Everything for your own folders:

- **Name**: words in any order (`final report` finds `Report FINAL v2.docx`); a word with `*` or `?` is a pattern for the whole name (`*.pdf`, `invoice-??.docx`). Letter case does not matter.
- **Extensions** (`pdf, docx`), **size** (under 1 MB … over 1 GB) and **modified** (today, last 7 or 30 days, last year, older than a year).
- Sort by name, folder, size or date. Double-click a file to open it, use its folder button to show it in Explorer, right-click for both and to copy its path.
- The first 500 results are shown; **Show more** brings the next ones. **Ctrl+F** brings you to this page from anywhere in the window.

The first search lists the folders, using only what the folder listing gives (name, size, date), without opening any file; every later search filters that list in memory, in milliseconds even with hundreds of thousands of files. What your rules move, rename, copy or delete (and what you restore from the journal) shows up at its new place right away, without listing the folders again. Changes made outside FilePilot show up when the list is redone: automatically after 5 minutes, when the rules' folders change, or with **Refresh**. The list is freed from memory 10 minutes after you leave the page. Folders of hidden rules are searched only while hidden rules are unlocked.

## Duplicates

The **Duplicate** condition matches a file that has an identical, older copy: `report (1).pdf` next to `report.pdf`. The original (the oldest copy) never matches. *The original is also searched for in* adds other folders to compare against, for example where downloads were sorted earlier, so a file downloaded again today is recognized as a duplicate of the one moved last month. Only the file in the rule's watched folder can be touched.

The **Duplicates** page finds groups of identical files in the folders you choose (by default those of the active rules), largest first, with the space you would free. You choose which copy to keep; every copy is compared byte by byte with the kept one before it goes to the Recycle Bin. These operations appear in the journal as *Duplicates (manual)* and can be restored.

How files are compared, cheapest step first:

1. size;
2. a fingerprint of the first 64 KB;
3. a fingerprint of the whole file;
4. before any action, a byte-by-byte comparison, read in chunks (large files never sit in memory) and stopped at the first difference.

Fingerprints are stored in a catalog (the `prints` table) and stay valid as long as a file's size and modification date do not change, so a folder is read in full only once. The daemon loads the catalog at startup, saves it after each pass and removes rows of deleted files daily; the window uses the same catalog. Measured on 15,000 files with 1,500 duplicates: 0.2 s per pass with the catalog warm.

## Confirmations

**Ask for confirmation** pauses a rule at that step. The file appears on the Home page (and the Journal tab) under *Waiting for confirmation*, with its full path, size, the rule, and for duplicates the original it matches. You can sort the list, open the file's folder with **Show**, then **Approve** (the next actions run) or **Reject** (nothing happens), one by one or all at once.

Until you answer, no rule touches the file. A file that changes or disappears before you answer is evaluated again. Requests survive restarts. When a rule no longer asks for confirmation (stopped, deleted, switched to simulation, or the step removed), its requests disappear and the files are evaluated again.

## Simulation

A rule with **Simulation only** does not touch any file. It writes in the journal what it would have done, with the status *Simulated*; a confirmation step counts as approved, so the journal shows the actions after it as well. Leave a new rule like this for a day or two, check the journal, then untick it: the same files are then processed for real. Rules below a simulated rule work normally.

## Journal and restore

Every operation is written to a write-ahead journal before it happens and marked as done after. If the PC stops in the middle of an operation (including a copy between drives), the operation is completed or rolled back at the next start: a file is never lost or duplicated.

The **Journal** tab lists operations with search, filters by rule, status and period, and pages. **Restore** puts a file back where it was, including from the Recycle Bin with its original extension; **Whole run** restores every operation of one pass. A restored file is not processed again by the rule unless it changes or you mark it as pending. If a file changed after the operation, FilePilot asks before restoring it.

The journal keeps 30, 90 (default) or 365 days, or everything (Settings). Old entries are removed at startup and daily; restores that remember where a file came from are kept.

## When a file is processed

- Temporary download files are ignored: `.crdownload`, `.part`, `.partial`, `.opdownload`, `.!qB`, `.!ut`, `.aria2` and others, and Firefox's empty placeholder (`x.pdf` next to `x.pdf.part`).
- A file must keep the same size and date for a while before it is processed: 5 seconds by default, configurable from *Immediately* up to an hour (Settings). A longer interval gives you time to open or rename a new file before it is moved.
- A file open in another program (a paused download, a document open in Word) waits; it is checked every 3 seconds, without errors in the journal.
- Temporary failures (a locked file, a missing drive) are retried after 1, 5, 15 and 60 minutes. Failed commands are not repeated.
- While paused, nothing is processed; on resume a full scan catches up.
- A folder moved into a watched folder (with subfolders) is processed with everything in it. If change events are lost (buffer overflow), the folder is rescanned.
- Scans use the metadata from the folder listing; values are read again only right before an action.

`tools\slow-download.ps1` simulates a slow download for testing this by hand.

## Safety

- FilePilot never opens the files it finds with other programs and never runs them. The *in use* check is an open for reading; content is read by FilePilot's own parsers, written in Rust without `unsafe`. The only way a found file would start is a *Run a program* action configured on the file itself, and the editor warns about that.
- The *downloaded from the internet* mark (Zone.Identifier) is kept when a file is moved on NTFS.
- FilePilot's own files are never touched by rules, even inside a watched folder.
- Overwrites, undone copies and undone extractions go through the Recycle Bin, so they can be undone as well.

## Hidden rules

Tick **Hidden** in the editor to keep a rule out of exports. With a password (Settings → Hidden rules), hidden rules also disappear from the list, the journal and the preview until you unlock them; unlocking lasts while the window is open. The daemon runs them either way, and saving from a locked window keeps them in place. The password is stored in `settings.json` as PBKDF2-HMAC-SHA256 (100,000 iterations, own salt).

> [!NOTE]
> The password hides rules, it does not encrypt them: `rules.orgrules` is still plain text.

## Settings

- **Language**: English or Romanian; the tray menu follows at the next daemon start.
- **Theme**: automatic, light or dark.
- **Start with Windows**: starts the daemon at sign-in, straight into the tray.
- **Process a new file**: the settle interval described above.
- **Keep the journal**: how long operations are kept.
- **Hidden rules**: the password.
- **Import / export** rules, the data folder and the version.

Rules are backed up when saved, at most once every 10 minutes; the last 30 backups are kept in `backups\`.

## Data files

All in `%APPDATA%\FilePilot` (override with the `FILEPILOT_DATA` environment variable, e.g. for a portable setup):

| File | Purpose |
|---|---|
| `rules.orgrules` | the rules (JSON); the window writes it atomically and the daemon reloads it by itself |
| `organizator.db` | SQLite in WAL mode: journal, processed-file state, pending confirmations, fingerprint catalog and content results (the name dates from before the rename) |
| `settings.json` | preferences shared by the window and the daemon |
| `daemon-status.json` | daemon status for the window and the tray: rules, unavailable folders, pause, today's counts, last error, scan progress |
| `daemon.lock` | ensures a single daemon |
| `pause` | the Unix time until which nothing is processed; `0` means until resumed; absent means active |
| `request-*.json` | requests from the window (e.g. *Mark as pending*); the daemon applies and deletes them |
| `backups\` | rule backups |

## Command line

```text
filepilot                                  run with the tray icon (normal use)
filepilot --no-tray                        run without the tray icon
filepilot rules.orgrules --once            one pass over a rules file, then exit
          [--db file.db] [--settle SECONDS] [--lang ro|en]
filepilot journal <file.db> [--limit N]    print the latest journal entries
filepilot undo <file.db> <id>              restore one operation
filepilot undo <file.db> --run <run>       restore a whole run
                                           (--force restores files changed since the operation)
```

## Installer

`FilePilot_<version>_x64-setup.exe` is an NSIS installer built by Tauri. It installs per user, without admin rights, into `%LOCALAPPDATA%\FilePilot`, with a Start menu shortcut, an optional desktop shortcut and an entry in *Installed apps*. It is in English or Romanian depending on the Windows language, and downloads WebView2 if it is missing.

- On install and update the daemon is stopped before files are copied and restarted afterwards if it was running. An open window is closed only after it asks about unsaved changes.
- An autostart entry that pointed to another copy (for example a portable one) now points to the installed daemon.
- Uninstalling stops the daemon and removes autostart. Your data in `%APPDATA%\FilePilot` stays unless you tick *Also delete the data*.
- Silent install: `setup.exe /S`; silent uninstall: `uninstall.exe /S`.
- Upgrading from *Organizator* (the name before 1.0): the old install is removed, autostart is carried over and the data folder moves from `%APPDATA%\Organizator` to `%APPDATA%\FilePilot` on first start. Rule files exported by Organizator import as they are.
- The executables are not code-signed, so SmartScreen may ask for *More info → Run anyway* the first time.

Configuration: `crates/app/tauri.bundle.json` (merged over `tauri.conf.json` only when packaging, so a plain `cargo build` does not need the daemon as an external binary) and `crates/app/windows/` (the `hooks.nsh` install steps and the English/Romanian texts).

## Architecture

| Crate | Role | Dependencies |
|---|---|---|
| `org-core` | rule model, conditions, templates and variables, dictionaries, dates (ro/en), PBKDF2 | — |
| `org-fs` | `Fs` trait, `StdFs`, `MemFs` with fault injection, safe cross-volume moves, fingerprints | — |
| `org-journal` | write-ahead journal, crash recovery, restore | — |
| `org-actions` | action executor | — |
| `org-content` | text extraction: UTF-8/UTF-16/Windows-1250, DOCX, XLSX, ODT, PDF (DEFLATE, ZIP, objects, ToUnicode fonts) | — |
| `org-engine` | scanning, stable-file detection, state, retries, duplicates | — |
| `org-portable` | `.orgrules` import/export, permanent ids | — |
| `org-paths` | data folder, instance lock | — |
| `org-store` | SQLite storage (schema 5, migrated automatically) | rusqlite |
| `org-sysfs` | the real Recycle Bin | trash |
| `org-daemon` | `filepilot.exe`: background process, tray icon | notify, chrono, tray-icon, windows-sys |
| `org-app` | `filepilot-ui.exe`: the window (plain HTML/CSS/JS in `crates/app/ui`) | tauri 2 and plugins |

The guarantees above are covered by tests: crash recovery at every step of every operation (including an interrupted cross-volume copy), restore without reprocessing, settle and in-use detection, retries, cross-folder duplicates, confirmations across restarts. Content extraction is tested on real documents (Word, LibreOffice, qpdf) and on thousands of corrupted variants without panicking.

## Releasing

Versions follow [Semantic Versioning](https://semver.org/).

1. Set the same version in `Cargo.toml` (`[workspace.package]`) and `crates/app/tauri.conf.json`.
2. Add a `## [x.y.z] - YYYY-MM-DD` section at the top of `CHANGELOG.md`.
3. Commit and push to `main`.

The Windows workflow checks that both version numbers match, runs the tests, builds the installer and tests it. If everything passes and `vx.y.z` has no release yet, it creates the tag and publishes a release named *FilePilot x.y.z* with the installer, a portable ZIP and the changelog section as release notes. Pushes to other branches, and to `main` without a new version, only build and test; their installer and portable ZIP are kept as workflow artifacts for 2 days. Larger features are developed on a branch (`feature/…`) and merged into `main` once tested. Pushing a `vx.y.z` tag by hand works too, as long as it matches the version.
