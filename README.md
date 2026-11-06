# Nanarchive

An archiver that aims to compress smaller than 7-Zip and WinRAR, with a fast, modern, soft rounded UI.
Archive format: **`.nax`**. See [PLAN.md](PLAN.md) for the full plan and roadmap.

## Project layout

| Folder | What it is |
|---|---|
| `crates/nax-core` | Compression engine and `.nax` format (Rust library) |
| `crates/nax-cli` | `nax` command-line tool |
| `app` | Desktop app: Svelte UI (`app/src`) + Tauri shell (`app/src-tauri`) |
| `bench` | Benchmark corpus and scripts (Phase 2) |

## Requirements

Rust (stable, MSVC), Visual Studio C++ Build Tools, Node.js LTS, WebView2 (built into Windows 10/11).

## Common commands

```bash
cargo test --workspace                  # run all tests
cargo build --release -p nax-cli        # build target/release/nax.exe
cd app && npm install && npm run tauri dev   # run the desktop app
powershell -ExecutionPolicy Bypass -File scripts\build-installer.ps1   # installer + latest.json
```

## Using the `nax` command-line tool

```bash
nax a photos.nax C:\Photos -p max       # create (presets: store, fast, balanced, max, ultra)
nax a keep.nax C:\Docs -p ultra         # smallest; extracting is as slow as packing
nax l photos.nax                        # list contents (instant)
nax t photos.nax                        # test: check every checksum
nax x photos.nax -o D:\Restore          # extract (existing files are kept; -y replaces)
nax x photos.nax "Photos/2024" -o out   # extract only some files/folders
nax a secret.nax C:\Docs -P             # encrypt with a password (asked twice, or NAX_PASSWORD)
nax a backup.nax C:\Data -r 5           # add a 5% recovery record
nax a big.nax C:\Video -v 700m          # split into 700 MB parts (big.part1.nax, ...)
nax repair backup.nax                   # fix damage using the recovery record
```

The format is documented in [docs/FORMAT.md](docs/FORMAT.md).

## Status

- Phase 0 (setup) — done
- Phase 1 (`.nax` format, engine, CLI) — done
- Phase 2 (benchmark + speed engine) — done; run `python -I bench\bench.py`, results in `bench/results/`
- Phase 3 (JPEG/PNG/ZIP recompression, dedup, file-type sorting, Max tuning) — done
- Phase 4 (desktop app: WinRAR-style file manager, English + Russian, Explorer right-click menu and .nax icon, opens ZIP) — done
- Phase 5 (AES-256 password encryption, recovery records + repair, split volumes, fuzzed and hardened reader) — done
- Phase 6 (Ultra = own context-mixing codec: smaller than 7-Zip Ultra and WinRAR Best in every benchmark category; extracting Ultra is slow) — done
- Phase 7 (opens 7z and RAR, Windows installer, signed auto-update from an address you choose) — done
- Phase 8 (video: MPEG-1 .mpg recompressed losslessly, ~16% smaller than 7z; MPEG-2 next) — in progress
