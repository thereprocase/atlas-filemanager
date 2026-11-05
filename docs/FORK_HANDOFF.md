# Atlas fork development handoff

Updated November 5, 2026.

The fork now has separate Windows wishlist and Gridline theme branches, an automatic collector into `dev`, and a successful Full Windows dev package. The package includes RoboRightClick as an optional companion. The exact reported Desktop Properties crash and the owner's sync-provider setup still need testing on the owner's computer.

## Repository and branches

Repository: [thereprocase/atlas-filemanager](https://github.com/thereprocase/atlas-filemanager). Upstream: [neondeex/atlas-filemanager](https://github.com/neondeex/atlas-filemanager). The work started at upstream commit `989508daef8aede39c5db2e955339a5894ed45f4`.

| Branch | Purpose | Feature tip in the successful dev build |
| --- | --- | --- |
| `main` | Collector, build workflows, packaging, and this handoff | Infrastructure through `4aad113` |
| `feature/wishlist` | Windows navigation, previews, bookmark dragging, Properties threading | `3ac70df` |
| `theme/gridline` | Gridline Light and Dark | `bacbe02` |
| `dev` | Collected integration branch and build source | `3a8539730a923532bcf125c99a6fa094daab9d79` |
| `build/windows-full-demo` | Earlier stock Full evaluation build | `04bd907` |

## Download and verification

The [successful dev run](https://github.com/thereprocase/atlas-filemanager/actions/runs/42056605567) built commit `3a8539730a923532bcf125c99a6fa094daab9d79`.

[Download its Windows artifact](https://github.com/thereprocase/atlas-filemanager/actions/runs/42056605567/artifacts/13432648890). The artifact is named `Atlas-Dev-Windows-x64` and contains:

- `Atlas-Dev-Windows-x64.zip`: Full Atlas, helper executables, notices, and the RoboRightClick companion.
- `AtlasSetup-0.1.0-dev.exe`: Atlas installer; RoboRightClick is supplied separately in the ZIP.
- `BUILD.json` and `SHA256SUMS.txt`: exact source, compiler, edition, companion commit, and file hashes.

The SHA-256 checksums and ZIP contents were verified after downloading. The Windows executable smoke test printed `Atlas 0.1.0-dev`.

Extract the ZIP completely and run `Start-Atlas.cmd` for the evaluation launcher, which disables automatic startup of the optional search helper. Running `atlas.exe` directly retains normal upstream startup behavior. Settings use the normal Atlas user configuration directory.

RoboRightClick is built from `thereprocase/robo-rightclick` commit `4ce1c1eb1ae239233ff41a15ec445257c0fb63e2`. Double-click its companion EXE and accept its per-user installation to register its shell commands. Atlas loads registered Windows shell commands into its context menu. Atlas Ctrl+C/Ctrl+V still uses Atlas; Robo's Ctrl+Shift+V hotkey targets Explorer and the desktop. Robo transfers are outside Atlas undo.

## Collector and builds

[collect-dev.yml](../.github/workflows/collect-dev.yml) runs when `feature/wishlist` or `theme/gridline` is pushed. It merges `main`, the wishlist tip, and the Gridline tip into `dev`, pushes without force, and dispatches [windows-dev.yml](../.github/workflows/windows-dev.yml). A merge conflict stops collection before publishing the merge.

For an infrastructure-only change on `main`, dispatch collection explicitly:

```sh
gh workflow run collect-dev.yml --repo thereprocase/atlas-filemanager --ref main
```

The Windows workflow builds the actual collected dev commit with Rust 1.97.1 and the existing optimized `release-perf` profile. The `free` feature is not enabled, so the edition is Full without a premium key. It builds Atlas and the search, cloud, and icon helpers together; verifies the pinned rclone payload; builds RoboRightClick with .NET SDK 10.0.111; packages the installer and ZIP; and records checksums. A newer dev run supersedes an older running dev build.

## Wishlist implementation and remaining checks

| Item | Implementation | Validation and remaining check |
| --- | --- | --- |
| This PC, Network, mapped drives, provider roots | Read-only shell locations; current-user logical drive enumeration; discovery of provider roots registered on the virtual Desktop | Windows test enumerates the system drive. Actual mapped drives, Nextcloud, and organization sync libraries need owner-machine testing. Network drives are excluded from the local-drive index option. |
| Recent to Bookmarks dragging | Navigation rows produce reference drags; bookmark headers, empty collections, and insertion lanes accept them | Compiles. Check dragging, reordering, and dropping outside the sidebar interactively. A Recent reference cannot remove an existing bookmark when dropped outside. |
| Desktop Properties crash | Native Properties calls run on a separate pumped STA shell worker; combined properties remain available for multiple selections | Windows build passes. The reported Desktop from Recent crash needs an interactive retry; its original cause was not established from a crash report. |
| XLSM preview | Spreadsheet recognition and preview routing include `.xlsm`; Calamine reads cells without running VBA | Windows test reads expected cells from an XLSM fixture containing an inert macro payload. |
| Missing PDF renderer | Windows first-page rendering uses the OS PDF renderer, with bounded output and optional pdftoppm fallback | Windows test renders one page from a two-page PDF and checks the requested size. |
| UNC paths opening Explorer | Existing directories use their actual type; UNC hosts become shell containers and shares remain filesystem locations | Classification tests pass. Check a real host, share, nested directory, and file on the owner's network. |

Shell locations provide no writable filesystem destination. Their navigation history and session restoration are supported. Network visibility and access still follow the current Windows user token and provider configuration.

## Gridline themes

Choose `Gridline Light` or `Gridline Dark` in the theme picker. They bundle IBM Plex Sans and Mono, use opaque surfaces, square main controls, solid blue active tabs with white labels, visible sidebar action buttons, and an 18 px base scale instead of 16 px. User zoom still applies. Dark follows the Bambu Bridge Gridline palette; light follows the system-gray Gridline palette. Some specialized Atlas widgets retain their own drawing behavior.

The clean local compile and scoped text-contrast checks passed. Both modes, zoom levels, and window layouts still need visual review in Windows.

## Validation scope

The successful Windows dev workflow passed the model edition/navigation tests, XLSM preview test, native namespace test, bounded PDF preview test, RoboRightClick Core tests, application/helper builds, executable smoke test, and packaging.

The inherited repository-wide formatting check already failed on the upstream snapshot. This work did not perform a repository-wide formatter cleanup; the dedicated dev workflow is the recorded functional build result.

Next interactive checks are the reported Desktop Properties action, Recent bookmark dragging, real network paths and provider roots, PDF/XLSM previews, Robo context commands, and both Gridline modes. These require the owner's actual Windows setup.
