# gbakit

A clean-room Game Boy Advance (GBA) format toolkit in Rust built on **[Formatkit](https://github.com/JidouGhost-TL/formatkit)** (`=0.1.2`).

- **Documentation & GBA Research Wiki:** **https://jidoughost-tl.github.io/gbakit/** (`Using gbakit`, 3,377-ROM corpus index, 175 reverse-engineered save-format maps (172 built into gbakit) with downloadable `.sav` / `save-format.json`, 35 verified interactive debug menu patches, 91 Engine/SDK communities, live VRAM/PRAM probes, and DirectSound PCM scans).

## Crates

| Crate | Responsibility |
| --- | --- |
| `gba-rom` | Checked 192-byte AGB headers, complement checksum, `0x08000000` bus mapping, SDK backup tags, ROM pointer-table scanner, Multiboot (`0x02000000`) helpers |
| `gba-compress` | Complete GBA BIOS codecs: LZ77 (`0x10` WRAM/VRAM), RLE (`0x30`), Huffman 4/8-bit decode & encode (`0x24`/`0x28`), `BitUnPack` (`SWI 0x10`), `Diff8`/`Diff16` (`0x81`/`0x82`) |
| `gba-gfx` | `BGR555` palettes, allocation-free 4bpp/8bpp tiles, OAM sprites & affine matrices (`ObjAttr`/`ObjAffine`), text BG compositor, Mode 3/4 framebuffers, 1bpp/2bpp font glyphs |
| `gba-sound` | DirectSound 16-byte `WaveData` PCM + `m4a` / MusicPlayer2000 voicegroups, song headers, song tables, and track sequence bytecode parser |
| `gba-save` | Declarative GBA battery-save schema engine, geometry detection, title probe, padding trimming, 25 checksum algorithms (`sum8`/`16`/`32`, word-sums, `xor8`/`16`/`32`, `sum_xor16`, `rol32_add`, `Crc16` incl. XMODEM), EEPROM `rev8` block reversal, verification, repair, and field decode/edit |
| `gba-saves` | Embedded registry of 172 clean-room verified GBA save definitions |
| `gba-patches` | Embedded registry of 35 clean-room verified GBA debug-menu re-enablement patches |
| `gbakit` / `gbakit-cli` | Facade crate and CLI (`info`/`inspect`, `rom tables`, `compress`, `save probe/trim/info/verify/decode/fix`, `sound wav/m4a`, `gfx tile4`, `patch apply/create/debug-menu/list-debug-menus`) |

## Save Identification, Trimming, and Geometry Detection

Identify loose `.sav` files, verify geometry, and strip trailing emulator padding:

```sh
# Probe loose .sav against embedded registry of 172 verified save formats
gbakit save probe loose.sav

# Display detailed geometry, padding byte, and slot layout
gbakit save info loose.sav

# Trim trailing 0xFF/0x00 padding down to native hardware geometry
gbakit save trim loose.sav -o clean.sav
```

## Unlock an Embedded Debug Menu

`gbakit` includes clean-room verified micro-patches (15–30 bytes) for 35 commercial GBA titles to re-enable dormant developer debug menus:

```sh
# List all 35 supported games and target addresses
gbakit patch list-debug-menus

# Unlock debug menu automatically by game code (outputs game.debug.gba)
gbakit patch debug-menu mygame.gba
```

All patches, verification galleries, and standalone `.ips` downloads remain available on the [GBA Reference Wiki Patches Gallery](https://jidoughost-tl.github.io/gbakit/patches/index.html).

## Apply or Create an IPS patch

```sh
# Apply external IPS patch
gbakit patch apply patch.ips original.gba patched.gba

# Create IPS patch from original and modified files
gbakit patch create original.gba modified.gba -o patch.ips
```

## Verification

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```
