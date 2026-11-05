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
```

## Using the `nax` command-line tool

```bash
nax a photos.nax C:\Photos -p max       # create (presets: store, fast, balanced, max, ultra)
nax l photos.nax                        # list contents (instant)
nax t photos.nax                        # test: check every checksum
nax x photos.nax -o D:\Restore          # extract (existing files are kept; -y replaces)
nax x photos.nax "Photos/2024" -o out   # extract only some files/folders
```

The format is documented in [docs/FORMAT.md](docs/FORMAT.md).

## Status

- Phase 0 (setup) — done
- Phase 1 (`.nax` format, engine, CLI) — done
- Next: Phase 2 — benchmark against ZIP, 7-Zip and WinRAR
