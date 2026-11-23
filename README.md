<p align="center">
  <img src="assets/icon-256.png" width="128" height="128" alt="MacCleanUp icon: an armadillo">
</p>

<h1 align="center">MacCleanUp</h1>

<p align="center">
  A native macOS cleanup app written in Rust. It shows where your disk space goes,<br>
  finds what is safe to clear, and moves nothing until you review it.
</p>

<p align="center">
  macOS 12 or later &nbsp;·&nbsp; Apple Silicon and Intel &nbsp;·&nbsp; MIT license
</p>

![The Storage view: every part of the startup disk with its size, and the details of the selected part](docs/screenshots/storage.png)

## What it does

- **Shows where the space goes.** A storage bar splits the startup disk into macOS, apps, your files, developer tools, device backups, Mail and Messages, caches, the Trash, swap, purgeable space, and System Data. Click any part to see what it holds.
- **Finds what is safe to clear.** Caches and logs, leftover installers, large files you have not touched in six months, and Cargo and npm build folders.
- **Uninstalls apps cleanly.** An app goes to the Trash with its exact caches and logs. Its settings and data go too, only if you tick the box.
- **Maps any folder.** A treemap draws a folder's contents to scale, and you can open any folder in it.
- **Never deletes.** Everything goes to the Trash after a review dialog that lists every path, so you can restore anything until you empty the Trash.

| Overview | Review before anything moves |
| --- | --- |
| ![Overview with the storage bar and its legend](docs/screenshots/overview.png) | ![The review dialog listing the one path that will move](docs/screenshots/review.png) |
| **Caches and logs** | **Build folders** |
| ![Caches and logs list with plain-language explanations](docs/screenshots/caches.png) | ![Cargo target and npm node_modules folders found in projects](docs/screenshots/build-folders.png) |

## Install

MacCleanUp has no installer. Build it from source with Rust, then open `build/<version>/MacCleanUp.app`:

```sh
git clone https://github.com/dominicferri/MacCleanUp.git
cd MacCleanUp
./scripts/build-app.sh
```

The script builds a universal app for Apple Silicon and Intel. The app needs nothing else on the Mac that runs it: no Rust, no Homebrew, no other packages. It uses the Avenir Next fonts that ship with macOS.

The app is ad-hoc signed, not notarized. If macOS blocks a copy that came from another Mac, open System Settings, then Privacy & Security, and click Open Anyway.

`MacCleanUp.app/Contents/MacOS/MacCleanUp --version` prints the version without opening a window.

## Views

A sidebar lists the views. Lists have a details panel on the right that shows where the item lives, its size, when it last changed, what it is, and what happens when it moves to the Trash. The status bar shows the last result, a progress bar for any running scan, and a Stop button.

- **Overview:** free space, and a storage bar. At launch the bar shows macOS (the System, Preboot, Recovery and Update volumes), Applications, Caches and logs, Swap and sleep image (the VM volume), and Purgeable space (Time Machine local snapshots and other space macOS frees by itself). **Measure your files and more** adds Your files (Desktop, Documents, Downloads, Movies, Music, Public, and Pictures without the Photos library), Developer tools (`~/Library/Developer`, `.cargo`, `.rustup`, `.npm`, and Homebrew, without the caches already listed), iPhone and iPad backups, Mail and Messages, and the Trash. The rest is System Data and other files. A part that macOS does not let MacCleanUp read is left out and named under the bar; Full Disk Access includes it. Below the bar, each cleanup area has its total and a button to review it.
- **Storage:** the same parts as a list. Click a part in the list, on the bar, or in the Overview legend to see what it holds: the macOS volumes, the largest apps and caches, the largest folders in Your files and Developer tools (click one to open it in the disk map), each device backup, the items in the Trash, swap in use, and the Time Machine local snapshots by date. System Data has a button that opens your Library in the disk map.
- **Caches and logs:** user caches, logs, temporary files, Xcode DerivedData, Xcode device support files (iOS, watchOS, tvOS, and visionOS), CoreSimulator caches, and the npm download cache. Every item starts unselected.
- **Applications:** apps in `/Applications` and `~/Applications`, including one nested level, with version, bundle ID, size, and last change. The details panel lists exactly what uninstall moves: the app plus its exact bundle-ID cache and log folders. A checkbox, off for every app, also moves the app's settings and data named after its exact bundle ID in `~/Library`: `Application Support/<id>`, `Preferences/<id>.plist`, `Saved Application State/<id>.savedState`, `HTTPStorages/<id>`, and `WebKit/<id>`. The panel lists each of these paths before you tick the box. Documents, containers, group containers, and folders with any other name always stay. Most browsers keep their profiles under another name, so those stay too; Opera keeps its profile in `Application Support/com.operasoftware.Opera`, so for Opera the option moves it. Containers are left out because macOS asks for permission before another app's container can be read or moved.
- **Installer files:** `.dmg`, `.pkg`, `.mpkg`, `.iso`, and `.xip` files, plus `.pkg` and `.mpkg` bundle folders, in `~/Downloads`, `~/Desktop`, `~/Documents`, `~/Public`, `~/Library/Downloads`, and iCloud Drive's Downloads folder, directly or one folder below. ZIP archives are not inspected.
- **Large old files:** files that use 1 GB or more on disk and have not changed for 180 days, anywhere in the home folder except `~/Library` and the Trash.
- **Build folders:** Cargo `target` folders that contain `CACHEDIR.TAG`, and npm `node_modules` folders next to a `package.json`, anywhere in the home folder except `~/Library` and the Trash.
- **Disk map:** the items in a folder drawn to scale as a treemap, with a sortable list below it. Click a tile or row for details; double-click a folder to open it. Use Up, the breadcrumb path, or the path field (`~` is accepted) to move around. Sizes of large folders are kept until you quit, so going back to a folder you already measured is fast. **Measure again** discards them, and any Trash operation discards the sizes it changed.
- **System:** processor, macOS version, memory, startup disk use, load average, and uptime, updated every 2 seconds, plus the size of the Trash and the number of Time Machine local snapshots. If macOS does not let MacCleanUp read the Trash, the size shows as unavailable, not as zero.
- **Activity:** every Trash operation since the app opened, with each path that moved and any failures.

Caches and Applications scan together when the app opens. Installer files, Large old files, Build folders, Disk map, and the storage measurement wait for a click, because they read Desktop, Documents, and Downloads, and macOS may ask MacCleanUp for access to those folders the first time. The searches do not open `~/Library`, the Trash, app bundles, or Photos libraries, and do not cross onto other volumes.

Lists can be filtered by name and sorted by name, size, or last change.

## Keyboard

| Key | Action |
| --- | --- |
| `Cmd-1` to `Cmd-9`, `Cmd-0` | Switch view |
| `Cmd-R` | Scan the current view again |
| `↑` / `↓` | Move through a list |
| `Space` | Select the highlighted item |
| `Esc` | Stop a running scan |

The review dialog lists every path that will move and its size. `Return` and `Esc` both cancel it; only a click on **Move to Trash** proceeds. A Trash operation cannot be stopped, and the window stays open until it finishes.

## Safety model

- No automatic deletion, permanent deletion, Trash emptying, or permanent-delete fallback.
- Scanning and selection happen before an explicit confirmation.
- Cleanup paths must remain inside fixed user-owned roots. Relative paths, `..`, control characters, root directories, and missing paths are rejected.
- Each kind of item has its own guard, checked again just before it moves. Large files and build folders must be inside the home folder, outside `~/Library` and the Trash, and not protected. A build folder must still contain `CACHEDIR.TAG`, or sit next to a `package.json`. App settings and data must be exactly one of the listed names inside the real `~/Library` folder.
- The disk map protects the home folder itself, its standard top-level folders, shell profiles, `.ssh`, `.gnupg`, `.vault`, `~/Library/Keychains`, and anything outside the home folder, including external volumes. The check ignores letter case.
- A bundle ID read from an app's `Info.plist` is used in a path only if it is one plain name: not empty, no `/`, no leading `.`, and no control characters.
- File and parent device/inode identities are captured during scanning and rechecked immediately before each Trash operation, including the caches, logs, and data that uninstall moves with an app. Changed or unknown identities are skipped.
- Directory sizing does not follow symlinks or cross mounted filesystems. Partial sizes, including app sizes, are shown in amber; unavailable bytes are never shown as an empty folder. One unreadable item in a disk map folder is skipped and listed, and the rest of the folder still shows.
- App caches and data stay if the app move fails, or if another unselected installed copy uses the same bundle ID. The uninstall preview and the operation use the same function, so they cannot disagree.
- Trash failures are reported item by item.
- When macOS answers that MacCleanUp has no permission to move an item, MacCleanUp asks Finder to move that one item to the Trash. This can happen in a Documents or Desktop folder that cloud sync manages. Finder may ask for an administrator password when the item belongs to another user. Put Back still works. The path goes to the script as an argument, not as script text. The first time, macOS asks whether MacCleanUp may control Finder. MacCleanUp never runs itself as root: a move made as root would go to root's Trash, where Put Back does not work.
- MacCleanUp does not terminate running apps. Close an app before removing its caches or uninstalling it.
- The only other programs MacCleanUp starts are `tmutil listlocalsnapshots /` and `diskutil apfs list -plist`, which read the snapshot list and the volume sizes, `open` and `open -R` for the Finder buttons, and `osascript` for the Finder fallback above. It runs no shell cleanup scripts and no privileged commands.

## Sizes and speed

Sizes are space used on disk (allocated blocks), and a file with several hard links counts once per measured item, as `du` reports. Sparse files count only the blocks they use. APFS clones that share blocks can still be counted more than once.

Scans measure folders on all processor cores. On a 10-core Apple Silicon Mac, a folder of about 780,000 files and 165 GB took 3.3 to 4.3 seconds to size, against 14.5 to 17.6 seconds for a single-threaded walk of the same folder, with files already in memory. The total matched `du -sk` to the byte. **Stop** ends a scan within a moment, and a stopped view stays unscanned until you scan it again.

## Build and test

```sh
cargo test
cargo clippy --all-targets -- -D warnings
./scripts/build-app.sh
```

The build creates `build/<version>/MacCleanUp.app` from a fresh staging folder, verifies its plist and ad-hoc signature, and checks that the binary contains `arm64` and `x86_64` slices. It refuses to overwrite an existing build of the same version.

The interface tests run headless through `egui_kittest` and never open a window. Set `MACCLEANUP_SHOTS=<folder>` to save a PNG of each tested screen; the screenshots on this page come from those tests.

In an iCloud-synced Desktop or Documents folder, macOS can attach `FinderInfo`, `macl`, or provenance attributes after packaging or first launch. A later strict signature check can then report `resource fork, Finder information, or similar detritus not allowed`, while the non-strict check passes.

## Source layout

- `src/scanner.rs`: bounded read-only inventories and searches, parallel on-disk sizing, containment, and identity checks
- `src/trash.rs`: reviewed operations through `NSFileManager.trashItemAtURL`, the Finder fallback, and the shared uninstall preview
- `src/system.rs`: macOS system metrics, volume use, the storage breakdown, Trash size, and local snapshots
- `src/gui/state.rs`: views, background jobs, selection, review dialogs, and the activity log, independent of drawing
- `src/gui/mod.rs`: window layout, views, dialogs, and headless interface tests
- `src/gui/theme.rs`: colors, fonts, and widget style
- `src/gui/treemap.rs`: squarified treemap layout for the disk map
- `src/main.rs`: command-line flags and window setup

<!-- git-understand:start -->
## Project map

MacCleanUp has two parts: a library that finds, sizes and guards files, and an egui window that drives it. All 39 links between the two point from the window into the library, so the library does not depend on the interface. The structure shown here comes from version 0.4.0, and the history is current.

- src/scanner.rs has the most incoming links (6). It holds the path guards and identity checks that src/trash.rs runs before every move.
- Only src/trash.rs moves anything to the Trash. The window reaches it through accept_confirmation in src/gui/state.rs.
- format_bytes in src/model.rs is the most called function, with 12 calls.
- src/gui/mod.rs is the largest hotspot, at 2,841 lines.

### Architecture

The GUI layer makes 39 links into the Core Library layer, and 32 of them are calls. No link goes back from the library to the GUI, so the library tests run without a window.

```mermaid
flowchart TD
  L0["GUI Application Layer<br/>5 files"]
  L1["Build, Packaging and Docs<br/>4 files"]
  L2["Core Library Layer<br/>5 files"]
  L0 -->|39| L2
  L1 -->|7| L0
  L1 -->|4| L2
```

### Dependencies

src/scanner.rs and src/model.rs are the two files that most others depend on. src/trash.rs calls the guard and identity functions in src/scanner.rs before each move.

6 of 14 files: the most connected ones, test files left out, joined by the 10 strongest of the 30 links between them.

```mermaid
flowchart LR
  subgraph G0["GUI Application Layer"]
    direction TB
    N1["mod.rs"]
    N3["state.rs"]
  end
  subgraph G2["Core Library Layer"]
    direction TB
    N2["scanner.rs"]
    N4["trash.rs"]
    N6["model.rs"]
    N9["system.rs"]
  end
  N1 --> N3
  N1 --> N6
  N4 --> N2
  N3 --> N2
  N3 --> N4
  N3 --> N9
  N1 --> N2
  N9 --> N2
  N3 --> N6
  N1 --> N4
```

### Composition

The Rust source is 7,409 lines in 10 files. Cargo.lock is most of the rest, at 4,638 lines.

42 files, 12503 lines.

| Language | Files | Lines |
|---|---:|---:|
| Rust | 10 | 7409 |
| Lockfile | 1 | 4638 |
| Docs | 3 | 241 |
| Other | 20 | 85 |
| Shell | 1 | 83 |
| TOML | 1 | 32 |
| Config | 3 | 12 |
| JavaScript | 2 | 2 |

### History

The history is short and comes from one day. scripts/build-app.sh changes with the source files because each release raises its version number, not because the code depends on it.

7 commits, 2026-11-23.

| Hotspot | Commits | Lines |
|---|---:|---:|
| `src/gui/mod.rs` | 5 | 2841 |
| `src/gui/state.rs` | 5 | 1437 |
| `src/scanner.rs` | 3 | 1589 |
| `src/system.rs` | 5 | 658 |
| `src/trash.rs` | 4 | 438 |
| `scripts/build-app.sh` | 6 | 83 |
| `src/model.rs` | 2 | 146 |
| `src/gui/theme.rs` | 2 | 134 |

src/gui/mod.rs, src/gui/state.rs and src/scanner.rs are complex, and together they hold 5,867 of the 7,409 Rust lines. Most of the risk sits in those three files.

| Changed together | Commits | Linked in code |
|---|---:|---|
| `scripts/build-app.sh` + `src/gui/mod.rs` | 5 | no |
| `scripts/build-app.sh` + `src/gui/state.rs` | 5 | no |
| `scripts/build-app.sh` + `src/system.rs` | 5 | no |
| `src/gui/mod.rs` + `src/gui/state.rs` | 5 | yes |
| `src/gui/mod.rs` + `src/system.rs` | 5 | no |
| `src/gui/state.rs` + `src/system.rs` | 5 | yes |
| `scripts/build-app.sh` + `src/trash.rs` | 4 | no |
| `scripts/build-app.sh` + `src/scanner.rs` | 3 | no |

<!-- git-understand:end -->

## Reference

Mole 1.74.0 (GPL-3.0) supplied the behavior and safety requirements reviewed for this independent implementation. No Mole scripts or binaries are bundled or invoked.

Version 0.4.4 does not implement Mole's Optimize, Purge, Slim, Touch ID, update, or administrator-only workflows.

## License

MIT
