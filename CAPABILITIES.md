# gbakit Capabilities

Generated directly from `gbakit capabilities --json` (`formatkit.capabilities.v1`) for **gbakit 0.2.0** (FormatKit `0.1.14`).

## Supported Formats

| ID | Category | Capabilities | Tier | Notes |
|---|---|---|---:|---|
| `rom.agb-header` | `rom` | parse, decode | 3 | 192-byte AGB cartridge header, complement checksum, SDK save tags, trailing padding, and peripheral signatures |
| `rom.multiboot` | `rom` | parse, decode, reproduce | 3 | Embedded 256 KiB EWRAM MultiBoot payload scanning, extraction, and serial link server |
| `code.arm-thumb-analysis` | `code` | parse, decode | 2 | ARM/Thumb pointer-table scanning, boot reachability trace, and callgraph extraction |
| `compress.bios-lz77` | `compress` | parse, decode, reproduce | 3 | GBA BIOS SWI 0x11/0x12 LZ77 (VRAM-safe and WRAM) and LZ11 codecs |
| `compress.bios-rle` | `compress` | parse, decode, reproduce | 3 | GBA BIOS SWI 0x14/0x15 run-length encoding |
| `compress.bios-huffman` | `compress` | parse, decode, reproduce | 3 | GBA BIOS SWI 0x13 4-bit and 8-bit Huffman codecs |
| `compress.bios-diff` | `compress` | parse, decode, reproduce | 3 | GBA BIOS SWI 0x16/0x18 8-bit and 16-bit differential filters |
| `compress.studio-codecs` | `compress` | parse, decode | 2 | Third-party GBA studio codecs (Hudson LZ, Konami dictionary, Shin'en Mode 7) |
| `gfx.gba-tiles-palettes` | `gfx` | parse, decode, reproduce, edit | 3 | BGR555 palettes, 4bpp/8bpp 8x8 tiles, OBJ sprites, and BG screenblock tilemaps (SCR/TMX) |
| `audio.m4a-sappy` | `audio` | parse, decode, reproduce, edit | 3 | MP2000/M4A voicegroups, song tables, PCM/DPCM/IMA-ADPCM sample replacement, and WAV/MIDI/SF2 export |
| `audio.arm-sound-engines` | `audio` | parse, decode | 2 | 13 GBA ARM audio engines (M4A, GAX, MusyX, Krawall, Konami KCE, Rare, Logik State, Southpaw, RD1 Kaede, Paragon5, EAD SMA, Engine SW, Cosmigo Sonix) with WAV/SF2 extraction |
| `video.mobiclip` | `video` | parse, decode | 3 | ActImagine Mobiclip MOC5 (GBA v1) and VXGB (GBA v2) video and audio streams |
| `ereader.dotcode` | `ereader` | parse, decode, reproduce, author | 3 | GBA e-Reader short/long DotCode strips, Reed-Solomon ECC, and BMP/PBM/PGM strip rendering |
| `ereader.vpk0` | `ereader` | parse, decode, reproduce, author | 3 | e-Reader VPK0 LZ/Huffman compression (methods 0 and 1) |
| `archive.pack` | `archive` | parse, decode, reproduce, edit | 2 | PACK studio container scanning, extraction, repacking, and in-ROM replacement |
| `archive.wayforward` | `archive` | parse, decode, reproduce, edit | 2 | WAYFORWARD container scanning, extraction, repacking, and in-ROM replacement |
| `archive.sfcd` | `archive` | parse, decode, reproduce, edit | 2 | SFCD container scanning, extraction, repacking, and in-ROM replacement |
| `text.charmap-scripts` | `text` | parse, decode, reproduce, edit | 2 | Charmap table scanning, string extraction, and pointer-aware in-ROM replacement |
| `text.huffman-scripts` | `text` | parse, decode, reproduce, edit | 2 | IS-style and Camelot-style Huffman compressed text extraction and replacement |
| `text.vwf-fonts` | `text` | parse, decode, reproduce, edit | 2 | 1bpp/2bpp variable-width bitmap font dumping, replacement, and glyph width tables |
| `save.gba-schema-v2` | `save` | parse, decode, reproduce, edit | 3 | Declarative Schema v2 save inspection, JSON export/import, field editing, checksum verification/repair, and canonical trimming across EEPROM 512B/8KB, SRAM 32KB, and Flash 64KB/128KB |
| `patch.ips-bps-ups` | `patch` | parse, decode, reproduce, author | 3 | IPS, BPS, and UPS binary patch creation and CRC-verified application |
| `link.gba-protocols` | `link` | parse, decode | 3 | Multi-Play 16-bit, MultiBoot, Normal SPI, GameCube JOYBus, and Wireless Adapter (AGB-015 / RFU) capture dissection and virtual link bridge |
| `map.asset-map-v1` | `map` | parse, decode, reproduce, author | 2 | Declarative formatkit.asset-map.v1 ROM asset map generation, merge, verification, Markdown reporting, and map-driven export |

## Save Engines

- `gba-save-v2`
- `eeprom-512b`
- `eeprom-8k`
- `sram-32k`
- `flash-64k`
- `flash-128k`

## Patch Formats

- `ips`
- `bps`
- `ups`
