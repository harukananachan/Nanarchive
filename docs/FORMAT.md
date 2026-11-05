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
| 2 | flags (0, reserved for encryption etc.) |
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
Allowed shapes: `[]` (stored raw), `[codec]`, `[LDM]`, `[LDM, codec]`.

| Step id | Name | Props |
|---|---|---|
| 1 | zstd | 1 byte: window log (10–31) |
| 2 | LZMA2 (raw) | 4 bytes: dictionary size (u32); optionally + 4 bytes: preset-dictionary length (u32); optionally + 1 byte flags (bit 0: x86 BCJ filter in front of LZMA2) |
| 3 | LDM (long-range matches) | u8 window log, varint payload length |

**Preset dictionary.** If the LZMA2 props carry a length `P > 0`, the coder is primed with the `P`
original stream bytes just before the block. Such blocks are decoded in stream order.

**x86 filter.** liblzma's BCJ x86 filter (start offset 0) runs before LZMA2. It is used for blocks
that are mostly Windows executables (files starting with `MZ`).

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
## Safety rules for readers

- Check magic, version, index hash and every block hash before trusting data.
- Every block's unpacked size and every file range must fit inside the stream.
- Paths are validated before writing: no absolute paths, drive letters, `..`, reserved Windows
  names (`CON`, `NUL`, …) or forbidden characters. Unsafe paths are refused.
- Decompression output is limited to the declared size.
