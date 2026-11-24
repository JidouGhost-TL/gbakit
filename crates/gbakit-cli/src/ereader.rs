//! CLI subcommands for Nintendo GBA e-Reader DotCode strips and `vpk0` compression.

use std::fs;
use std::path::Path;

use gba_ereader::dotcode::{
    bin_to_raw, build_bin, parse_bin, raw_to_bin, CardHeader, CARD_HEADER_LEN, LONG_STRIP_BLOCKS,
    LONG_STRIP_MAX_PAYLOAD, SHORT_STRIP_BLOCKS, SHORT_STRIP_MAX_PAYLOAD, STRIP_BLOCK_DATA,
};
use gba_ereader::render::render_strip;
use gba_ereader::vpk0::{vpk0_compress_with_method, vpk0_decompress};

/// Strip length selection for `gbakit ereader encode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StripSize {
    Auto,
    Short,
    Long,
}

fn write_output(out_path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("create dir {}: {e}", parent.display()))?;
        }
    }
    fs::write(out_path, bytes).map_err(|e| format!("write {}: {e}", out_path.display()))
}

fn build_long_bin(
    region: u8,
    card_type: u8,
    strip_index: u8,
    strip_count: u8,
    payload: &[u8],
) -> Result<Vec<u8>, String> {
    if payload.len() > LONG_STRIP_MAX_PAYLOAD {
        return Err(format!(
            "payload length {} exceeds long strip capacity ({LONG_STRIP_MAX_PAYLOAD} bytes)",
            payload.len()
        ));
    }
    let mut out = vec![0u8; CARD_HEADER_LEN + LONG_STRIP_MAX_PAYLOAD];
    out[CARD_HEADER_LEN..CARD_HEADER_LEN + payload.len()].copy_from_slice(payload);
    let header = CardHeader {
        region,
        card_type,
        strip_index,
        strip_count,
        payload_len: payload.len() as u16,
        flags: 0x01,
        data_checksum: 0,
        global_checksum: 0,
    };
    let hdr_bytes = header.to_bytes(&out[CARD_HEADER_LEN..]);
    out[..CARD_HEADER_LEN].copy_from_slice(&hdr_bytes);
    Ok(out)
}

/// `gbakit ereader decode <strip.raw> -o <out.bin> [--decompress]`
pub fn cmd_ereader_decode(
    strip_path: &Path,
    out_path: &Path,
    decompress: bool,
) -> Result<(), String> {
    let raw = fs::read(strip_path).map_err(|e| format!("read {}: {e}", strip_path.display()))?;
    let bin = raw_to_bin(&raw).map_err(|e| format!("decode strip {}: {e}", strip_path.display()))?;

    if decompress {
        let (header, payload) =
            parse_bin(&bin).map_err(|e| format!("parse decoded .bin: {e}"))?;
        let decompressed = vpk0_decompress(payload)
            .map_err(|e| format!("vpk0 decompress payload: {e}"))?;
        write_output(out_path, &decompressed)?;
        println!(
            "e-Reader Decode + Decompress: {} ({} bytes raw -> {} bytes .bin -> {} bytes vpk0 payload -> {} bytes decompressed, strip {}/{})",
            out_path.display(),
            raw.len(),
            bin.len(),
            payload.len(),
            decompressed.len(),
            header.strip_index + 1,
            header.strip_count,
        );
    } else {
        let (header, payload) =
            parse_bin(&bin).map_err(|e| format!("parse decoded .bin: {e}"))?;
        write_output(out_path, &bin)?;
        println!(
            "e-Reader Decode: {} ({} bytes raw -> {} bytes .bin, payload {} bytes, strip {}/{})",
            out_path.display(),
            raw.len(),
            bin.len(),
            payload.len(),
            header.strip_index + 1,
            header.strip_count,
        );
    }
    Ok(())
}

/// `gbakit ereader encode <in.bin> -o <out.raw> [--short|--long]`
pub fn cmd_ereader_encode(
    in_path: &Path,
    out_path: &Path,
    size: StripSize,
) -> Result<(), String> {
    let input = fs::read(in_path).map_err(|e| format!("read {}: {e}", in_path.display()))?;
    let short_bin_len = SHORT_STRIP_BLOCKS * STRIP_BLOCK_DATA;
    let long_bin_len = LONG_STRIP_BLOCKS * STRIP_BLOCK_DATA;

    let bin_bytes: Vec<u8> = match parse_bin(&input) {
        Ok((header, payload)) => match size {
            StripSize::Auto => input,
            StripSize::Short => {
                if input.len() > short_bin_len {
                    return Err(format!(
                        "input .bin ({} bytes) exceeds short strip size ({short_bin_len} bytes)",
                        input.len()
                    ));
                }
                input
            }
            StripSize::Long => {
                if input.len() == long_bin_len {
                    input
                } else {
                    build_long_bin(
                        header.region,
                        header.card_type,
                        header.strip_index,
                        header.strip_count,
                        payload,
                    )?
                }
            }
        },
        Err(_) => match size {
            StripSize::Auto => build_bin(1, 0, 0, 1, &input)
                .map_err(|e| format!("build .bin strip: {e}"))?,
            StripSize::Short => {
                if input.len() > SHORT_STRIP_MAX_PAYLOAD {
                    return Err(format!(
                        "payload ({} bytes) exceeds short strip capacity ({SHORT_STRIP_MAX_PAYLOAD} bytes)",
                        input.len()
                    ));
                }
                build_bin(1, 0, 0, 1, &input).map_err(|e| format!("build short .bin strip: {e}"))?
            }
            StripSize::Long => build_long_bin(1, 0, 0, 1, &input)?,
        },
    };

    let raw = bin_to_raw(&bin_bytes).map_err(|e| format!("encode .raw strip: {e}"))?;
    write_output(out_path, &raw)?;
    let kind = if raw.len() == gba_ereader::SHORT_STRIP_RAW_LEN {
        "short"
    } else {
        "long"
    };
    println!(
        "e-Reader Encode: {} ({} bytes .bin -> {} bytes {kind} .raw strip)",
        out_path.display(),
        bin_bytes.len(),
        raw.len(),
    );
    Ok(())
}

/// `gbakit ereader render <strip.raw> -o <out.bmp|pbm|pgm> [--dpi 300|600|1200]`
pub fn cmd_ereader_render(
    strip_path: &Path,
    out_path: &Path,
    dpi: u32,
) -> Result<(), String> {
    let raw = fs::read(strip_path).map_err(|e| format!("read {}: {e}", strip_path.display()))?;
    let bmp = render_strip(&raw, dpi)
        .map_err(|e| format!("render strip at {dpi} DPI: {e}"))?;

    let ext = out_path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();

    let encoded = match ext.as_str() {
        "bmp" => bmp.to_bmp(),
        "pbm" => bmp.to_pbm(),
        "pgm" => bmp.to_pgm(),
        _ => {
            return Err(format!(
                "unsupported output format for {}: expected .bmp, .pbm, or .pgm",
                out_path.display()
            ));
        }
    };

    write_output(out_path, &encoded)?;
    println!(
        "e-Reader Render: {} ({}x{} px, {dpi} DPI, {}, {} bytes)",
        out_path.display(),
        bmp.width,
        bmp.height,
        ext.to_ascii_uppercase(),
        encoded.len(),
    );
    Ok(())
}

/// `gbakit ereader vpk-decode <in.vpk> -o <out.bin>`
pub fn cmd_ereader_vpk_decode(in_path: &Path, out_path: &Path) -> Result<(), String> {
    let data = fs::read(in_path).map_err(|e| format!("read {}: {e}", in_path.display()))?;
    let out = vpk0_decompress(&data)
        .map_err(|e| format!("vpk0 decode {}: {e}", in_path.display()))?;
    write_output(out_path, &out)?;
    println!(
        "e-Reader vpk0 Decode: {} ({} bytes -> {} bytes)",
        out_path.display(),
        data.len(),
        out.len(),
    );
    Ok(())
}

/// `gbakit ereader vpk-encode <in.bin> -o <out.vpk> [--method 0|1]`
pub fn cmd_ereader_vpk_encode(
    in_path: &Path,
    out_path: &Path,
    method: u8,
) -> Result<(), String> {
    if method > 1 {
        return Err(format!("invalid vpk0 method {method}: expected 0 or 1"));
    }
    let data = fs::read(in_path).map_err(|e| format!("read {}: {e}", in_path.display()))?;
    let out = vpk0_compress_with_method(&data, method)
        .map_err(|e| format!("vpk0 encode {}: {e}", in_path.display()))?;
    write_output(out_path, &out)?;
    println!(
        "e-Reader vpk0 Encode (method {method}): {} ({} bytes -> {} bytes)",
        out_path.display(),
        data.len(),
        out.len(),
    );
    Ok(())
}
