<div align="center">

<img src="docs/images/logo.png" alt="Atlas" width="88" height="88">

# Atlas

**The file manager at the speed of thought.**

A native, GPU-accelerated, local-first file manager for Windows. Built with Rust and [GPUI](https://www.gpui.rs/).

[![Website](https://img.shields.io/badge/atlasnative.com-18181B?style=for-the-badge&logo=googlechrome&logoColor=white)](https://atlasnative.com)
[![Microsoft Store](https://img.shields.io/badge/Get_it_on-Microsoft_Store-0078D4?style=for-the-badge&logo=microsoft&logoColor=white)](https://apps.microsoft.com/detail/9MX95D1GDMR3)
[![Discord](https://img.shields.io/badge/Discord-5865F2?style=for-the-badge&logo=discord&logoColor=white)](https://discord.gg/aeAvQ27bgG)

[![License: AGPL-3.0-or-later](https://img.shields.io/badge/License-AGPL--3.0--or--later-blue.svg)](LICENSE)
[![Built with Rust](https://img.shields.io/badge/Built_with-Rust-orange.svg)](https://www.rust-lang.org/)
![Status: alpha](https://img.shields.io/badge/status-alpha-ff5a1f.svg)

<br>

<img src="docs/images/hero.webp" alt="Atlas grid view with file-type icons" width="860">

</div>

<br>

## Why Atlas

Atlas draws every pixel on the GPU and keeps your files on your machine. Folders open instantly, scrolling stays smooth with very large directories, and everything is reachable from the keyboard.

> **Status:** Atlas is in free alpha on Windows 10/11. Expect rough edges and fast iteration. A Linux build is in development; macOS is on the roadmap.

## Features

- **Five views** — Grid, List, Details, Miller columns and Tree, chosen per pane.
- **Tabs and split panes** — split in any direction, drag and drop between panes, reopen closed tabs.
- **Command palette** — `Ctrl+Shift+P` runs any command; `Ctrl+K` jumps to any folder with fuzzy search.
- **Instant search** — an indexed, fuzzy search that finds files as you type.
- **Preview** — press `Space` to preview images, video, audio, documents, code and archives.
- **Local conversion** — convert images, video, audio and documents from the context menu or a radial picker. Files never leave your PC.
- **Archives as folders** — browse ZIP, RAR, 7z and TAR without extracting.
- **Searchable context menu** — your own actions and the native Windows shell menu in one overlay.
- **Tags, filters and profiles** — color tags, saved layouts and per-profile favorites.
- **Private folders** — password-protected folders.
- **Disk cleanup** — find what is using space and clean it safely.
- **Cloud and network** — Google Drive, OneDrive and other providers via rclone, plus SMB, FTP and WebDAV, right in the sidebar.
- **Extensions** — optional first-party extensions (icon themes, thumbnails, Pandoc, Opus, cloud drives, document previews).
- **Themes and icon packs** — including VS Code icon themes.

<div align="center">
<img src="docs/images/miller.webp" alt="Miller columns with a live preview pane" width="860">
<br><sub>Miller columns with a live preview.</sub>
<br><br>
<img src="docs/images/convert.webp" alt="Radial format picker for local conversion" width="860">
<br><sub>Convert files locally from a radial format picker.</sub>
<br><br>
<img src="docs/images/grid.webp" alt="Grid view with tabs" width="860">
<br><sub>Tabs, breadcrumbs and a grid with real file-type icons.</sub>
</div>

## Install

- **Microsoft Store (recommended):** [Atlas on the Microsoft Store](https://apps.microsoft.com/detail/9MX95D1GDMR3). Free to install; updates are automatic.
- **Website:** [atlasnative.com](https://atlasnative.com) has the latest news, the changelog and the Linux waitlist.

Atlas is free to use. An optional one-time Premium license unlocks higher limits (more panes, profiles, cloud connections, conversions and batch rename); see [atlasnative.com](https://atlasnative.com/#pricing).

## Keyboard shortcuts

Shortcuts can be remapped in Settings.

| Shortcut | Action |
|---|---|
| `Ctrl+Shift+P` | Command palette |
| `Ctrl+K` | Go to folder |
| `Ctrl+T` / `Ctrl+W` / `Ctrl+Shift+T` | New / close / reopen tab |
| `Ctrl+Tab`, `Ctrl+1`–`9` | Switch tab |
| `Ctrl+Shift+S` / `Ctrl+Alt+S` | Split horizontally / vertically |
| `F6`, `Ctrl+Alt+←→↑↓` | Move between panes |
| `Alt+←` / `Alt+→` / `Alt+↑` | Back / forward / up |
| `Ctrl+F`, `Ctrl+Shift+F` | Search, search in subfolders |
| `Space`, `Ctrl+I` | Preview / inspector |
| `F2` | Rename |
| `Ctrl+Z` / `Ctrl+Shift+Z` | Undo / redo |
| `Ctrl+Alt+C` | Convert |
| `Ctrl+,` | Settings |

## Windows wishlist branch (fork)

This branch adds Windows shell browsing for This PC, Network and UNC hosts, discovery of drive-letter mappings and registered Desktop provider roots, dragging navigation references into Bookmarks, native first-page PDF rendering, and XLSM cell previews. Properties commands run on a separate pumped STA shell worker. XLSM preview reads workbook data through Calamine and does not run VBA. Shell locations supply no filesystem destination for paste or drag-and-drop.

Windows runtime checks on the owner's mapped drives and organization sync providers remain necessary. Namespace enumeration depends on registered Windows shell providers; it does not invent or configure cloud accounts. The Properties change removes a UI re-entry path; confirmation against the reported Desktop crash remains pending.


## Build from source

Requirements: Windows 10/11, the MSVC build tools (Visual Studio Build Tools with the Windows SDK) and [Rust](https://rustup.rs/). The repository pins the toolchain in [`rust-toolchain.toml`](rust-toolchain.toml), so `cargo` picks the right version automatically.

```powershell
git clone https://github.com/neondeex/atlas-filemanager.git
cd atlas-filemanager

cargo run --release          # run Atlas
cargo check --workspace      # type-check everything
cargo test --workspace       # run the tests
```

The first build compiles GPUI and takes a while. Useful aliases live in [`.cargo/config.toml`](.cargo/config.toml). To try the opt-in Free limits, see [Build editions](docs/build-editions.md).

## Project layout

| Path | What it holds |
|---|---|
| `crates/atlas-model` | Shared data types and application state |
| `crates/atlas-core` | File system, search, preview, conversion, archives, cloud |
| `crates/atlas-ui` | The GPUI application: views, overlays, input, drag and drop |
| `crates/atlas-win-space` | Windows disk-space and cleanup engine |
| `crates/atlas-search-service` | Background search index service |
| `extensions/` | First-party declarative and sidecar extensions |
| `vendor/` | Patched forks of GPUI and gpui-component (Apache-2.0) |
| `docs/` | Guides for contributors and extension authors |

Read [ARCHITECTURE.md](ARCHITECTURE.md) for the dependency rules and [docs/extensions.md](docs/extensions.md) to write an extension.

## Contributing

Bug reports, ideas and pull requests are welcome. Please read [CONTRIBUTING.md](CONTRIBUTING.md) first, open an [issue](https://github.com/neondeex/atlas-filemanager/issues) to discuss larger changes, and join the [Discord](https://discord.gg/aeAvQ27bgG) to talk to the developers. To report a vulnerability, see [SECURITY.md](SECURITY.md).

## License

Atlas is licensed under the [GNU AGPL v3.0 or later](LICENSE). Third-party components keep their own licenses; see [NOTICE](NOTICE) and [`legal/THIRD-PARTY-NOTICES.txt`](legal/THIRD-PARTY-NOTICES.txt).

The Atlas name and logo are not covered by the AGPL license.
