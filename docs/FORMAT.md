# `.nax` format, version 1

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
similar data sits together). The stream is cut into blocks (size depends on preset); each block is
compressed independently, so blocks are compressed and decompressed in parallel.

A file is just a range `[stream_offset, stream_offset + size)` of the stream. Ranges may span
several blocks (large files) and several files may share a block (solid compression). Ranges are
allowed to overlap, which future deduplication relies on.

Blocks are stored back to back starting at offset 16; a block's position is the sum of the packed
sizes before it, and the total must end exactly at `index_offset`.

### Codec chain

Each block has a chain of steps applied in order when compressing (reversed when extracting).
An empty chain means stored raw.

| Step id | Name | Props |
|---|---|---|
| 1 | zstd | 1 byte: window log (10–31) |
| 2 | LZMA2 (raw) | 4 bytes: dictionary size (u32) |

Future versions add filters (BCJ, delta, JPEG, deflate re-packing…) as new step ids.

## Index (zstd-compressed)

```
varint  index_version (= 1)
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
varint  entry_count
repeat entry_count:
    varint+bytes  path (UTF-8, `/` separators, relative, no `.`/`..`)
    u8      kind (0 = file, 1 = folder)
    u8      flags (bit 0: has mtime)
    ivarint mtime in nanoseconds since 1970-01-01 UTC (if flag bit 0)
    varint  attributes (Windows file attributes)
    if file:
        varint  size
        varint  stream_offset
        32 B    BLAKE3 hash of the file contents
```

## Safety rules for readers

- Check magic, version, index hash and every block hash before trusting data.
- Every block's unpacked size and every file range must fit inside the stream.
- Paths are validated before writing: no absolute paths, drive letters, `..`, reserved Windows
  names (`CON`, `NUL`, …) or forbidden characters. Unsafe paths are refused.
- Decompression output is limited to the declared size.
