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
cargo test                              # run core tests
cargo run -p nax-cli -- --help          # the nax CLI
cd app && npm install && npm run tauri dev   # run the desktop app
```

## Status

Phase 0 (setup) done. Next: Phase 1 — `.nax` reader/writer and the CLI.
