<p align="center"><img src="app/static/logo.svg" width="96" alt=""></p>

<h1 align="center">Nanarchive</h1>

<p align="center"><b>A Windows archiver that makes smaller archives than 7-Zip and WinRAR.</b><br>
Own archive format <code>.nax</code> · English and Russian · public domain (Unlicense)</p>

> **Vibecoded with Claude Opus 5.5**, an AI model by Anthropic. All code, tests and documents in this
> project were written by Claude Opus 5.5 in Claude Code.

## What is Nanarchive?

Nanarchive packs files and folders into one smaller file, like WinRAR or 7-Zip, but it tries
harder to make them small:

- **Photos (JPEG)** are re-encoded without losing a single bit, about 20% smaller. Other
  archivers save about 1% on photos.
- **PNG, ZIP, DOCX, XLSX, PDF and other "already compressed" files** are unpacked inside the
  archive, compressed properly, and rebuilt exactly when you extract.
- **MPEG-1 video (.mpg)** is re-encoded without loss, about 16% smaller than 7-Zip.
- **Text, source code and programs** use its own context-mixing compressor in Ultra mode.
- **Duplicate files** are stored once.

Every file comes back **bit for bit identical**. Nanarchive checks this while it packs.

It also has:
- a desktop app that looks and works like WinRAR (file manager, Add / Extract / Test / Info)
- AES-256 passwords, recovery records with repair, and archives split into parts
- opening ZIP, 7z and RAR archives
- the Explorer right-click menu, and an installer with signed auto-updates

## Test results

The same files packed with each archiver at its strongest setting (smaller is better):

| Test files | Nanarchive Ultra | 7-Zip Ultra | WinRAR Best |
|---|---:|---:|---:|
| JavaScript code | **14.43 MB** | 17.86 MB | 19.23 MB |
| Rust code | **10.85 MB** | 13.85 MB | 17.38 MB |
| HTML documents | **4.86 MB** | 5.49 MB | 6.42 MB |
| Photos (JPEG) | **36.31 MB** | 45.58 MB | 45.75 MB |
| PNG images | **26.62 MB** | 32.85 MB | 33.15 MB |
| Programs (.exe, .dll) | **27.13 MB** | 29.04 MB | 34.79 MB |
| ZIP files | **25.74 MB** | 34.00 MB | 35.29 MB |
| **Total** | **145.94 MB** | 178.67 MB | 192.01 MB |
| MPEG-1 video clip (22.87 MB) | **18.46 MB** | 21.99 MB | 22.20 MB |

Overall that is **18% smaller than 7-Zip** and **24% smaller than WinRAR**.

Trade-off: Ultra is slow, and extracting it takes as long as packing. The **Max** preset is much
faster and still beats 7-Zip on photos, PNGs and ZIPs. Videos in modern formats (MP4/MKV with
H.264/H.265) are already compressed as tightly as possible, so no archiver shrinks them much.

**Quality checks.** There are 72 automatic tests. They check that files come back identical,
that wrong passwords and damaged archives are refused cleanly, and that repair works. A fuzzer
fed the archive reader about 740,000 damaged archives with no crash. Run the tests with
`cargo test --workspace`.

## Download

Build it yourself (see below). The installer is made by `scriptsuild-installer.ps1`.

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

## License

Public domain ([Unlicense](LICENSE)): use, change, share or sell the code for anything, no credit needed. RAR archives are opened with the UnRAR library, which may only be used to extract RAR files, never to create them.
