# Nanarchive — Master Plan

> Nanarchive (nana + archive): an archiver that aims to compress smaller than 7-Zip / RAR / ZIP
> and feel nicer to use — fast, light, with a modern soft rounded UI.
> Archive extension: **`.nax`**

---

## 1. Reality check: how do we beat 7z and RAR?

7-Zip (LZMA2) and RAR5 are very good general compressors. Writing a new general algorithm
that beats LZMA2 everywhere is not realistic in a first version. We win differently:

| Trick | What it does | Typical gain vs 7z |
|---|---|---|
| **Per-file smart codec choice** | Detect file type, pick the best codec for it (zstd / LZMA2 / store / specialist) | small, but never worse |
| **JPEG recompression** (Lepton) | Losslessly re-encodes JPEG photos, bit-exact on extract | **~20–23% smaller** on JPEGs — 7z/RAR get ~1% |
| **Deflate un-wrapping** (preflate) | Unpacks the deflate stream inside PNG / ZIP / DOCX / PDF, compresses the raw data properly, rebuilds bit-exact on extract | **10–40%** on PNG, Office docs, PDFs, nested zips |
| **Executable filters** (BCJ/x86, ARM64) | Makes .exe/.dll code more compressible | same as 7z (needed to match it) |
| **Content-defined dedup** | Finds repeated chunks across all files (copies, backups, game assets) | large on duplicate-heavy data, which RAR/7z handle badly beyond their window |
| **Solid blocks sorted by type** | Groups similar files together before compressing | a few % |
| **"Ultra" mode: context mixing** (later) | A slow, very strong model for text/logs/source code | 15–30% smaller than 7z on text, but much slower |

Promise we can honestly make: **"never bigger than 7z on Ultra, and a lot smaller on photos,
documents, PNGs, and duplicate-heavy folders."** We prove it with a public benchmark (section 7).

---

## 2. Tech stack

| Part | Choice | Why |
|---|---|---|
| Compression core | **Rust** | As fast as C, memory-safe (archivers parse untrusted files — safety matters), great libraries |
| UI shell | **Tauri 2** | Native window using Windows' built-in WebView2. ~5–10 MB app, low RAM, unlike Electron (~150 MB) |
| UI framework | **Svelte 5 + TypeScript** | Tiny, compiled, very fast — ideal for a smooth, animated UI |
| CLI | `nar` command (Rust, same core) | For power users, scripts, benchmarks |

Key Rust crates: `zstd`, `xz2` / `liblzma` (LZMA2), `lepton_jpeg` (Microsoft, Apache-2.0),
`preflate-rs` (Microsoft, Apache-2.0), `blake3` (checksums), `rayon` (multi-core),
`chacha20poly1305` / `aes-gcm` + `argon2` (encryption), `zip`, `sevenz-rust`, `unrar` (reading other formats).

**Tools to install first:** Rust (rustup), Visual Studio C++ Build Tools, Node.js LTS, Git.

---

## 3. The `.nax` format (v1)

Design goals: fast listing, safe parsing, recoverable, extensible.

```
[Header]       magic "NANA", format version, flags
[Blocks...]    compressed data blocks (solid groups or single files)
               each block: codec id + filter chain + sizes + BLAKE3 hash
[Index]        file tree: names, sizes, times, attributes, which block/offset
               (compressed, optionally encrypted)
[Recovery]     optional Reed-Solomon parity (repair damaged archives, like RAR)
[Footer]       index offset + hash + magic  → open & list instantly without reading blocks
```

- **Codec chain per block**, e.g. `preflate → lzma2` or `lepton` or `bcj-x86 → lzma2`.
  New codecs just get a new id — old archives keep working.
- **Integrity**: BLAKE3 hash for every block and file.
- **Encryption**: XChaCha20-Poly1305, key from password via Argon2id. Optionally encrypt file names too.
- **Split volumes**: `name.nax`, `name.x01`, `name.x02`…
- **Safety**: protect against path traversal (`../`), zip bombs (size limits), symlink tricks.

---

## 4. Compression presets (what the user sees)

| Preset | Engine | Use |
|---|---|---|
| **Store** | none | already-compressed video, etc. |
| **Fast** | zstd level 3, multithread | quick backups, ~ZIP size but 5–10× faster |
| **Balanced** (default) | zstd 19 long-window + JPEG/PNG specialists | best everyday trade-off |
| **Max** | LZMA2 + filters + dedup + all specialists | beat 7z Ultra |
| **Ultra** (phase 6) | Max + context-mixing for text | smallest possible, slow |

Video/audio/already-compressed files are auto-detected and stored (no wasted CPU).

---

## 5. UI design

Look & feel: soft, rounded, calm. Large radius corners (12–20 px), soft shadows, gentle
gradients, Windows 11 Mica-style translucent background, smooth 60 fps animations,
light & dark themes, accent color picker.

**Screens**

1. **Home** — big drop zone "Drop files to compress or a .nax to open", recent archives.
2. **Create archive** — name, preset slider (Fast ↔ Ultra) with *live size estimate*, password,
   split size, recovery %. One big "Compress" button.
3. **Archive browser** — file tree like Explorer, search, preview of images/text inside,
   drag files out to extract, drag in to add.
4. **Progress** — circular progress, speed, ratio, time left, "saved X MB" celebration at the end.
5. **Settings** — theme, file associations, Explorer context menu, language.

**Performance rules for "very optimized"**
- Virtualized lists (100,000+ files scroll smoothly).
- All work in Rust background threads; the UI never freezes.
- Instant start (< 300 ms), small installer.

**Windows integration**
- Explorer right-click menu: "Compress with Nanarchive", "Extract here", "Extract to folder\".
- Double-click `.nax` opens it. Custom `.nax` file icon.

---

## 6. Roadmap (phases)

**Phase 0 — Setup** ✅ done
Install toolchain, create Git repo, Rust workspace: `nax-core`, `nax-cli`, `nax-app` (Tauri), app icon.

**Phase 1 — Core format + CLI** ✅ done
(First result: Max is 6% smaller than WinRAR Best on a 95 MB code folder, but ~15× slower — speed work later.)
`.nax` writer/reader, zstd + LZMA2, solid blocks, BLAKE3, multi-thread.
CLI: `nar a`, `nar x`, `nar l`, `nar t` (add / extract / list / test).
✅ Done when: round-trip any folder bit-exact, list a big archive instantly.

**Phase 2 — Benchmark harness**
A test corpus (photos, docs, source code, exes, game assets, PNGs, logs, mixed folder)
and a script comparing Nanarchive vs ZIP / 7z / RAR on size + speed.
✅ Done when: one command prints a comparison table. Every later phase is measured here.
✅ **Done.** `python -I bench\bench.py`. The engine was also rebuilt for speed: parallel LZMA2 blocks sized to the CPU count, a long-range matcher across blocks, a small preset dictionary, the x86 filter for .exe/.dll, parallel file reading and a compact index.
Result (Balanced vs WinRAR Best): smaller on code-js, code-rust, docs-html and programs, and faster on code-js, code-rust and docs-html. Still 0.4 s slower on programs and 0.5% bigger on JPEGs (Phase 3: JPEG recompression). Max/Ultra don't beat 7z Ultra on size yet except code-js (Phase 3 + 6).

**Phase 3 — The "beat 7z" features**
Executable filters, file-type detection & sorting, Lepton JPEG, preflate (PNG/ZIP/DOCX/PDF), dedup.
✅ Done when: Max preset beats 7z Ultra on the mixed corpus.
✅ **Done** (results: `bench/results/20261105-211211-phase3.md`). Lossless JPEG recompression (Lepton), deflate recompression for PNG / ZIP / JAR / DOCX / GZ (preflate, verified by full decode), content sniffing, sort by (extension, path), whole-file dedup, x86 filter, parallel chunked LZMA2 for huge files, long-match search in Max.
Result (MB, 7z Ultra → Nax Max / Nax Ultra): code-js 17.86 → **17.56 / 17.38**, code-rust 13.85 → 14.19 / **13.84**, docs 5.49 → 6.09 / 5.51, photos 45.58 → **36.31 / 36.29**, png 32.85 → **27.97 / 26.30**, programs 29.04 → 31.11 / 30.79, zips 34.00 → **26.22 / 25.59**.
Balanced beats WinRAR Best on size on all 7 sets. Max beats 7z Ultra on 4 of 7 sets and is 2–4× faster. Remaining gaps (Max on code-rust, docs; both on programs, where 7z uses BCJ2) → Phase 6 (stronger model for Ultra, BCJ2-style exe filter).

**Phase 4 — UI (Tauri + Svelte)**
All screens from section 5, themes, animations, progress events from the core.
✅ Done when: create / open / extract work fully from the UI and feel smooth.
Layout (chosen by the user): WinRAR-style file manager, modernized — menu bar in the title bar
(File / Commands / Options / Help), big soft toolbar (Add, Extract to, Test, View, Open, Find, Info,
Settings) with pastel icons, address bar (type a path, or `archive.nax\folder`), one file list that
browses disk folders and the inside of archives (".." goes up, out of the archive back to its folder),
status bar. Dialogs: Add to archive (preset slider + live size estimate), Extract (destination,
keep/replace), Archive information, Settings (theme, accent, labels, animations, default preset),
preview of images/text inside an archive, progress ring with speed / time left / "saved X MB".
Virtual list (100,000+ rows), drag & drop, right-click menu, keyboard shortcuts (Alt+A/E/T/V/I,
Ctrl+O/F/A, F5, Backspace, Enter, type-to-jump). `npm run dev` in a plain browser uses mock data.
Pulled forward from Phase 7 (user request): Explorer right-click menu and .nax icon (HKCU only,
toggle in Settings), quick actions that run in a small window and close when done (`--add`,
`--add-quick`, `--extract-here`, `--extract-to`, `--test`; one instance merges the files Explorer
sends), opening ZIP / JAR (browse, preview, extract, test). Time estimate uses measured speeds per
file type (JPEG and PNG/ZIP recompression are much slower than plain data on Max / Ultra).

**Phase 5 — Security & robustness**
Encryption, recovery records, split volumes, fuzz testing the reader, path-safety checks.

**Phase 6 — Ultra mode: beat 7z and WinRAR on everything**
Goal: Ultra is smaller than 7z Ultra **and** WinRAR Best on every category of the benchmark
corpus — not only photos/docs, but also text, source code, logs, executables and mixed folders.
- Context-mixing (CM) codec: many small prediction models (order-N context, word model,
  structure models for exe/text/tables) mixed by a neural mixer, then arithmetic coding.
- Specialized models per data type (x86 code, text/words, BMP/WAV raw data, tables/CSV).
- Multi-threaded by splitting into blocks so it stays usable on many-core CPUs.
- Tune with the benchmark until every row of the table is a win; track speed too.
✅ Done when: the benchmark shows Ultra smaller than both 7z and WinRAR in every category.

**Phase 7 — Windows integration & release**
Opening 7z / RAR (extract only) — ZIP and the Explorer menu are already done in Phase 4,
installer (MSI/NSIS via Tauri), auto-update.

---

## 7. How we'll know it's "the best"

- Benchmark table in the README: size and speed vs ZIP, 7z (Ultra), RAR5 (Best) on the same corpus.
- Round-trip tests on every build: extracted files must be bit-identical.
- Fuzzing the `.nax` reader so broken/malicious archives can't crash it.

---

## 8. Risks

| Risk | Plan |
|---|---|
| Specialist codecs (Lepton/preflate) are slow on huge files | Only in Balanced/Max; parallelize per file |
| Ultra mode can take very long to tune | It's the last phase; product is complete without it |
| RAR extraction license (unrar) forbids making RAR files | We only *extract* RAR, never create — allowed |
| WebView2 missing on old Windows 10 | Tauri installer bootstraps it |
