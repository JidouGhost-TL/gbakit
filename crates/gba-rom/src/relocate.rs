//! Canonical GBA ROM free-space discovery and ROM bus pointer relocation primitives.

#![deny(unsafe_code)]

use formatkit_core::{Error, Result};

use crate::{rom_offset_to_bus, HEADER_SIZE, MAX_ROM_SIZE};

/// Minimum safe ROM offset when falling back to `0x00`-filled free space.
///
/// The region `0xC0..0x200` often contains zero-initialized boot/header tables,
/// so `0x00` free-space searches must start at or past `0x200`.
pub const SAFE_ZERO_FREE_SPACE_START: usize = 0x200;

fn align_up_checked(value: usize, align: usize) -> Option<usize> {
    if align <= 1 {
        return Some(value);
    }
    let rem = value % align;
    if rem == 0 {
        Some(value)
    } else {
        value.checked_add(align - rem)
    }
}

/// Find a contiguous run of `needed_len` bytes matching any fill byte in `fill_bytes`
/// (searched in the order given), aligned to `alignment`, starting at or after
/// `start_offset`, clamped to [`MAX_ROM_SIZE`], and avoiding `exclude_range`.
///
/// Returns `None` if `needed_len == 0`, if arithmetic would overflow, or if no
/// suitable region exists within `rom.len().min(MAX_ROM_SIZE)`.
pub fn find_free_space(
    rom: &[u8],
    needed_len: usize,
    alignment: usize,
    start_offset: usize,
    fill_bytes: &[u8],
    exclude_range: Option<(usize, usize)>,
) -> Option<usize> {
    if needed_len == 0 {
        return None;
    }
    let limit = rom.len().min(MAX_ROM_SIZE);
    if limit < needed_len {
        return None;
    }
    let align = alignment.max(1);
    let base_start = align_up_checked(start_offset, align)?;
    if base_start.checked_add(needed_len).is_none_or(|end| end > limit) {
        return None;
    }

    for &fill in fill_bytes {
        let mut pos = base_start;
        while let Some(end) = pos.checked_add(needed_len) {
            if end > limit {
                break;
            }
            if let Some((ex_start, ex_end)) = exclude_range {
                if pos < ex_end && end > ex_start {
                    let Some(next) = align_up_checked(ex_end, align) else {
                        break;
                    };
                    pos = next;
                    continue;
                }
            }
            if let Some(mismatch) = rom[pos..end].iter().position(|&b| b != fill) {
                let Some(after) = pos.checked_add(mismatch).and_then(|p| p.checked_add(1)) else {
                    break;
                };
                let Some(next) = align_up_checked(after, align) else {
                    break;
                };
                pos = next;
            } else {
                return Some(pos);
            }
        }
    }
    None
}

/// Find free space in `rom`, preferring erased NOR Flash (`0xFF`) starting at
/// `start_offset.max(HEADER_SIZE)` before falling back to `0x00` starting at
/// `start_offset.max(SAFE_ZERO_FREE_SPACE_START)`.
pub fn find_free_space_prefer_ff(
    rom: &[u8],
    needed_len: usize,
    alignment: usize,
    start_offset: usize,
    exclude_range: Option<(usize, usize)>,
) -> Option<usize> {
    find_free_space(
        rom,
        needed_len,
        alignment,
        start_offset.max(HEADER_SIZE),
        &[0xFF],
        exclude_range,
    )
    .or_else(|| {
        find_free_space(
            rom,
            needed_len,
            alignment,
            start_offset.max(SAFE_ZERO_FREE_SPACE_START),
            &[0x00],
            exclude_range,
        )
    })
}

/// Rewrite all 4-byte-aligned Wait State 0 ROM bus pointers (`0x08xxxxxx`)
/// from `old_offset` to `new_offset` across `HEADER_SIZE..rom.len().min(MAX_ROM_SIZE)`,
/// skipping any 4-byte word overlapping `skip_range`.
///
/// Validates both `old_offset` and `new_offset` via [`rom_offset_to_bus`] before
/// mutating `rom`. Returns the sorted list of ROM byte offsets that were patched.
pub fn patch_rom_pointers(
    rom: &mut [u8],
    old_offset: usize,
    new_offset: usize,
    skip_range: Option<(usize, usize)>,
) -> Result<Vec<usize>> {
    let old_bus = rom_offset_to_bus(old_offset).ok_or(Error::InvalidField {
        what: "old_rom_offset",
        value: old_offset as u64,
    })?;
    let new_bus = rom_offset_to_bus(new_offset).ok_or(Error::InvalidField {
        what: "new_rom_offset",
        value: new_offset as u64,
    })?;

    let old_bytes = old_bus.to_le_bytes();
    let new_bytes = new_bus.to_le_bytes();
    let mut patched = Vec::new();

    let scan_end = rom.len().min(MAX_ROM_SIZE) & !3;
    let mut pos = HEADER_SIZE;
    while pos < scan_end {
        if let Some((skip_start, skip_end)) = skip_range {
            if pos < skip_end && pos + 4 > skip_start {
                let Some(next) = align_up_checked(skip_end, 4) else {
                    break;
                };
                pos = next.max(pos + 4);
                continue;
            }
        }
        if rom[pos..pos + 4] == old_bytes {
            rom[pos..pos + 4].copy_from_slice(&new_bytes);
            patched.push(pos);
        }
        pos += 4;
    }
    Ok(patched)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_free_space_handles_zero_len_zero_align_and_overflow() {
        let rom = vec![0xFFu8; 0x400];
        assert_eq!(find_free_space(&rom, 0, 4, 0x200, &[0xFF], None), None);
        // alignment == 0 treated as 1
        assert_eq!(
            find_free_space(&rom, 16, 0, 0x201, &[0xFF], None),
            Some(0x201)
        );
        // Overflow in start_offset / align_up / needed_len must return None, never panic or loop
        assert_eq!(
            find_free_space(&rom, 16, 4, usize::MAX, &[0xFF], None),
            None
        );
        assert_eq!(
            find_free_space(&rom, 16, 4, usize::MAX - 2, &[0xFF], None),
            None
        );
        assert_eq!(
            find_free_space(&rom, 16, usize::MAX, 0x201, &[0xFF], None),
            None
        );
        assert_eq!(
            find_free_space(&rom, usize::MAX, 4, 0x200, &[0xFF], None),
            None
        );
        assert_eq!(
            find_free_space(&rom, 16, 4, 0x200, &[0xFF], Some((0x200, usize::MAX))),
            None
        );
    }

    #[test]
    fn find_free_space_clamps_to_32mib_cap() {
        let mut rom = vec![0xAAu8; MAX_ROM_SIZE + 64];
        rom[MAX_ROM_SIZE..MAX_ROM_SIZE + 64].fill(0xFF);
        assert_eq!(
            find_free_space(&rom, 32, 4, MAX_ROM_SIZE - 16, &[0xFF], None),
            None
        );
    }

    #[test]
    fn find_free_space_prefer_ff_respects_safe_zero_start_and_exclude() {
        let mut rom = vec![0xAAu8; 0x600];
        // 0x00 region in 0xC0..0x200 must be ignored by prefer_ff fallback
        rom[0xC0..0x200].fill(0x00);
        assert_eq!(find_free_space_prefer_ff(&rom, 32, 4, 0, None), None);

        // 0x00 region at 0x200..0x240 is eligible
        rom[0x200..0x240].fill(0x00);
        assert_eq!(find_free_space_prefer_ff(&rom, 32, 4, 0, None), Some(0x200));

        // 0xFF region at 0x100..0x140 is preferred over 0x00 at 0x200
        rom[0x100..0x140].fill(0xFF);
        assert_eq!(find_free_space_prefer_ff(&rom, 32, 4, 0, None), Some(0x100));

        // Excluding 0x100..0x140 falls back to 0x00 at 0x200
        assert_eq!(
            find_free_space_prefer_ff(&rom, 32, 4, 0, Some((0x100, 0x140))),
            Some(0x200)
        );
    }

    #[test]
    fn patch_rom_pointers_validates_before_mutating_and_skips_range() {
        let mut rom = vec![0u8; 0x400];
        let old_off = 0x200usize;
        let new_off = 0x300usize;
        let old_bus = rom_offset_to_bus(old_off).unwrap();
        let new_bus = rom_offset_to_bus(new_off).unwrap();

        rom[0x100..0x104].copy_from_slice(&old_bus.to_le_bytes());
        rom[0x120..0x124].copy_from_slice(&old_bus.to_le_bytes());
        rom[0x140..0x144].copy_from_slice(&old_bus.to_le_bytes());

        // Invalid new_offset leaves ROM untouched
        let snapshot = rom.clone();
        assert!(patch_rom_pointers(&mut rom, old_off, MAX_ROM_SIZE, None).is_err());
        assert_eq!(rom, snapshot);

        // Skip range overlapping 0x120 (even unaligned start 0x122..0x126)
        let patched = patch_rom_pointers(&mut rom, old_off, new_off, Some((0x122, 0x126))).unwrap();
        assert_eq!(patched, vec![0x100, 0x140]);
        assert_eq!(&rom[0x100..0x104], &new_bus.to_le_bytes());
        assert_eq!(&rom[0x120..0x124], &old_bus.to_le_bytes());
        assert_eq!(&rom[0x140..0x144], &new_bus.to_le_bytes());
    }
}
