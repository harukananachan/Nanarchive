# `.nax` format, version 1 (index version 2)

All integers are little-endian. `varint` = unsigned LEB128 (7 bits per byte, low bits first,
max 10 bytes). `ivarint` = zigzag-encoded signed varint.

```
+------------------+  offset 0
| Header (16 B)    |
+------------------+  offset 16
| Block 0 (packed) |
| Block 1 (packed) |
| ...              |
+------------------+  index_offset
| Index (zstd)     |
+------------------+  file_len - 64
| Footer (64 B)    |
+------------------+
```

## Header (16 bytes)

| Size | Field |
|---|---|
| 4 | magic `NANA` |
| 2 | format version (`1`) |
| 2 | flags: bit 0 = encrypted (`FLAG_ENCRYPTED`); other bits must be 0 |
| 8 | reserved, zero |

## Footer (64 bytes, at the very end)

| Size | Field |
|---|---|
| 8 | `index_offset` |
| 8 | `index_packed_len` |
| 8 | `index_raw_len` |
| 32 | BLAKE3 hash of the packed index bytes |
| 2 | format version |
| 2 | flags |
| 4 | magic `NANA` |

A reader opens the footer first, so listing an archive never touches the data blocks.

## Data stream and blocks

All file contents are concatenated into one logical **stream** (files sorted by type/name so
similar data sits together). The stream is cut into blocks (size depends on preset and CPU count); blocks are compressed in
parallel. Blocks may refer back to earlier stream data (long-range matches, preset dictionaries),
which the reader keeps while extracting in order.

A file is just a range `[stream_offset, stream_offset + size)` of the stream. Ranges may span
several blocks (large files) and several files may share a block (solid compression). Ranges are
allowed to overlap, which future deduplication relies on.

Blocks are stored back to back starting at offset 16; a block's position is the sum of the packed
sizes before it, and the total must end exactly at `index_offset`.

### Codec chain

Each block has a chain of steps applied in order when compressing (reversed when extracting).
Allowed shapes: `[]` (stored raw), `[codec]`, `[LDM]`, `[LDM, codec]`, `[RECOMP]`, `[RECOMP, codec]`.
A codec is zstd, LZMA2 or CM.

| Step id | Name | Props |
|---|---|---|
| 1 | zstd | 1 byte: window log (10–31) |
| 2 | LZMA2 (raw) | 4 bytes: dictionary size (u32); optionally + 4 bytes: preset-dictionary length (u32); optionally + 1 byte flags (bit 0: x86 BCJ filter in front of LZMA2) |
| 3 | LDM (long-range matches) | u8 window log, varint payload length |
| 4 | RECOMP (recompressed files) | varint payload length |
| 5 | CM (context mixing, Ultra) | 3 bytes: version (`1`), flags (bit 0: x86 call transform), hash table size (log2 of buckets, 16–24) |

**Preset dictionary.** If the LZMA2 props carry a length `P > 0`, the coder is primed with the `P`
original stream bytes just before the block. Such blocks are decoded in stream order.

**x86 filter.** liblzma's BCJ x86 filter (start offset 0) runs before LZMA2. It is used for blocks
that are mostly Windows executables (files starting with `MZ`).

**RECOMP payload.** Whole files inside the block (JPEG, PNG, ZIP-family, GZip) are replaced by a
re-encoded form that rebuilds the exact original bytes:

```
varint  segment_count
repeat: varint gap (raw bytes before the segment), varint original length, u8 kind, varint encoded length
bytes   all raw bytes, in order
bytes   all encoded segments, in order
```

Kinds: 1 = JPEG (Lepton), 2 = PNG, 3 = ZIP, 4 = GZip. Kinds 2–4 use the "unpacked deflate"
layout from `crates/nax-core/src/zipped.rs`: a piece list (raw / deflate / PNG IDAT group), then
raw bytes, decompressed data and preflate corrections. IDAT CRCs are recomputed when rebuilding.

Kind 5 = MPEG-1 video (program stream `.mpg` or bare `.m1v`), defined by
`crates/nax-core/src/mpeg.rs` (version 1). The video stream is taken out of its packets; every
slice is parsed down to macroblock types, motion vectors, block patterns and DCT coefficients, and
these are coded with a context-mixing binary arithmetic coder (contexts from the neighbouring and
previous-picture blocks). Slices that do not re-encode to the exact same bits stay raw; MPEG-2
video is not touched. Layout:

```
u8      version (1)
varint  side raw length, varint side packed length, bytes side (LZMA)
varint  chunk_count, chunk_count x varint chunk length, then the coded chunks
```

The side data holds the packet skeleton (all bytes except video payload), where video payload goes
back in, and per chunk the list of units (raw bytes, or a slice start code with its zero padding).
Chunks are at least 2 MB, start at a sequence header and are coded independently (in parallel).
The encoder decodes its own output and keeps it only if the bytes match.
A big LZMA2 payload may be several independently compressed pieces joined into one stream (each
starts with a dictionary reset), which is plain LZMA2 to the decoder.

**Duplicates.** Identical files are stored once; their entries share the same stream range.

**CM (step 5).** Bitwise context mixing, defined by the code in `crates/nax-core/src/cm.rs`
(version 1); the decoder must run the identical model, so the code is the specification. In short:
each bit is predicted from 14 hashed contexts (orders 2, 3, 5, 8, two "indirect" contexts, word,
word pair, column, four sparse byte pairs, the byte a match predicts), stored as bit histories in
32-byte buckets and mapped to probabilities by learned tables, plus order 0/1 counters and a match
model (last occurrence of the previous 7 bytes). Two neural mixers (weights chosen by order 0 +
match state, and by the previous byte) and a final mixer combine them; three APM/SSE stages refine
the result; a 32-bit binary arithmetic coder (12-bit probabilities, carry-less, 4-byte flush) codes
it. Integer math only. With the x86 flag, the block was first transformed: at every `E8`/`E9` byte
the following 32-bit operand, if in `[-2^24, 2^24)`, gets the position after it added (mod 2^25,
staying in that range), and the scan always skips those 4 bytes. Blocks are independent; readers
refuse CM blocks over 64 MiB. Decoding is as slow as encoding (well under 1 MB/s per thread).

**LDM payload** (the input of the codec step, or the block itself for `[LDM]`):

```
varint  literal_count
bytes   literals
repeat until end:
    varint  literal run (literals copied before this match)
    varint  distance (back from the current stream position; may reach into earlier blocks)
    varint  length
```

Remaining literals follow the last match. A match may reach back at most `2^window_log` bytes
plus the block size, so a reader keeps that much recent stream data while extracting.

## Index (zstd-compressed)

```
varint  index_version (= 2)
varint  block_count
repeat block_count:
    varint  unpacked_size
    varint  packed_size
    varint  step_count
    repeat step_count:
        u8      step id
        varint  props_len
        bytes   props
    32 B    BLAKE3 hash of the packed block bytes
-- entries, stored column by column --
varint  folder_count                      folder table, sorted, front-coded:
repeat: varint shared_prefix_len, varint+bytes rest
varint  entry_count
repeat: varint folder id                  parent folder of each entry ("" = top level)
repeat: varint shared_prefix_len, varint+bytes rest     name, front-coded vs previous entry
repeat: u8 flags                          bit 0: folder, bit 1: has mtime
repeat (entries with mtime): ivarint      mtime ns since 1970 UTC, delta to previous mtime
repeat: varint attributes                 Windows file attributes
repeat (files): varint size
repeat (files): ivarint                   stream_offset minus end of previous file (usually 0)
repeat (files): 8 B                       first 8 bytes of the BLAKE3 hash of the contents
```

Path = `folder + "/" + name` (or just `name` at the top level): UTF-8, `/` separators, relative.

## Encryption (header flag bit 0)

An encrypted archive has a 64-byte **encryption header** right after the header, so blocks start
at offset 80 instead of 16.

| Offset | Size | Field |
|---|---|---|
| 0 | 1 | KDF id (`1` = Argon2id) |
| 1 | 1 | cipher id (`1` = AES-256-GCM) |
| 2 | 1 | Argon2 lanes `p` (1–64) |
| 3 | 1 | zero |
| 4 | 4 | Argon2 memory in KiB `m` (up to 2 GiB) |
| 8 | 4 | Argon2 passes `t` (1–32) |
| 12 | 16 | random salt (new for every archive) |
| 28 | 16 | password check value |
| 44 | 20 | zero |

- `master = Argon2id(password, salt, m, t, p)` (32 bytes). Defaults: 128 MiB, 3 passes, 4 lanes.
- `key = BLAKE3.derive_key("Nanarchive 2026-11 data key", master)`,
  `check = BLAKE3.derive_key("Nanarchive 2026-11 password check", master)[..16]`.
  A wrong password is detected by `check` without touching any data.
- Every packed block, and the zstd-compressed index, is sealed with AES-256-GCM and stored as
  `ciphertext || 16-byte tag`. Nonce (12 B) = `purpose u32 LE || counter u64 LE`: purpose 1 =
  block (counter = block number), purpose 2 = index (counter 0). Blocks can't be swapped or reused.
- Associated data = header (16 B) + encryption header (64 B), so changing the flags, KDF settings
  or salt makes every block fail.
- Block hashes in the index and the index hash in the footer cover the stored (encrypted) bytes,
  so `test` and repair work without the password. File names and sizes are inside the encrypted
  index and stay hidden. The footer repeats the flags.

## Split archives (volumes)

A split archive is one `.nax` byte stream cut into parts of equal size (the last may be shorter):
`name.part1.nax`, `name.part2.nax`, … (numbers zero-padded to the same width, e.g. `part01`).
The reader joins the parts in order; a missing part is reported by number. An archive that fits
into one part is written as plain `name.nax`.

## Recovery record (optional)

Reed–Solomon parity (GF(2^16), `reed-solomon-simd`) over everything before it — header, blocks,
index and footer, `protected_len` bytes. It is appended after the footer:

```
[header] [parity] [header again] [trailer 48 B]
```

The protected data is split into `N` data shards of `S` bytes (last one zero-padded), with
`S = max(4096, ceil(L / 16384))` rounded up to 64 and `N <= 16384`. There are
`R = ceil(N * percent / 100)` parity shards (1..N). Shards are coded in column slices of width
`W` bytes (`W = floor(256 MiB / (N + R))` rounded down to 64, clamped to 64..S) to keep memory low;
parity is stored slice by slice. Up to `R` damaged or missing shards can be rebuilt.

**Header** (both copies identical):

```
8 B   magic "NAXRECV1"
u64   protected_len
u32   shard size S
u32   data shards N
u32   parity shards R
u32   slice width W
16 B  x (N + R)   BLAKE3 of each shard, first 16 bytes (data shards, then parity shards)
32 B  BLAKE3 of everything above
```

**Trailer** (last 48 bytes): magic `NAXRTAIL`, `protected_len`, `S`, `N`, `R` (as above), 12 zero
bytes, then 8 bytes of BLAKE3 over the first 40 bytes.

A normal reader uses the trailer only to know where the archive itself ends. Repair finds a header
through the trailer or, if the trailer is damaged, by scanning for `NAXRECV1` at a position that is
consistent with its fields; it checks every shard and writes a new `name.repaired.nax` (or
`.repaired.partN.nax` for split archives). The original is never modified. In a split archive the
record spans the parts like the rest of the data, so a whole lost part can be rebuilt when the
record is large enough.

## Safety rules for readers

- Check magic, version, index hash and every block hash before trusting data.
- Every block's unpacked size and every file range must fit inside the stream.
- Paths are validated before writing: no absolute paths, drive letters, `..`, reserved Windows
  names (`CON`, `NUL`, …) or forbidden characters. Unsafe paths are refused.
- Decompression output is limited to the declared size.
- Unknown header flags or non-zero reserved bytes are refused. KDF settings read from an archive
  are capped (memory 2 GiB, 32 passes, 64 lanes) so a crafted file can't exhaust the machine.
- Extraction resolves each parent folder and requires it to stay inside the destination, so
  junctions or symlinks planted there can't redirect writes; existing symlinks are replaced,
  never written through.
- The reader is fuzzed (`cargo run --release -p nax-core --example fuzz -- <secs> <dir>`):
  damaged archives must give errors, never panics or writes outside the destination.
