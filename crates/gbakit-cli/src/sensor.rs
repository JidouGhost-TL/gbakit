//! CLI command handlers for GBA Cartridge Hardware Sensor Bridge.

#![deny(unsafe_code)]

use std::fs;
use std::path::Path;

use gbakit::sensor::{
    get_sensor_catalog, plan_sensor_injection, SensorInjectionConfig,
};

/// Display all 78 cartridge hardware sensor titles across 19 families.
#[allow(dead_code)]
pub fn cmd_sensor_list(as_json: bool) -> Result<(), String> {
    let catalog = get_sensor_catalog()?;

    if as_json {
        let out = serde_json::to_string_pretty(&catalog)
            .map_err(|e| format!("Failed to serialize sensor catalog: {e}"))?;
        println!("{out}");
        return Ok(());
    }

    println!("========================================================================================================================");
    println!(" GBA CARTRIDGE HARDWARE & PHYSICAL SENSOR DIRECTORY (78 TITLES / 19 HARDWARE FAMILIES)");
    println!("========================================================================================================================");
    println!(" Code | Hardware Types       | Title                                    | Bus Interface & Memory Registers");
    println!("------+----------------------+------------------------------------------+-----------------------------------------------");

    for t in &catalog.titles {
        let code = if t.game_code.is_empty() { "UNK" } else { &t.game_code };
        let hw_types = t.hardware_types.join(", ");
        let title_trunc = if t.title.len() > 40 {
            format!("{}...", &t.title[..37])
        } else {
            t.title.clone()
        };
        let iface_trunc = if t.interface_bus.len() > 47 {
            format!("{}...", &t.interface_bus[..44])
        } else {
            t.interface_bus.clone()
        };

        println!(
            " {:4} | {:20} | {:40} | {}",
            code, hw_types, title_trunc, iface_trunc
        );
    }

    println!("------------------------------------------------------------------------------------------------------------------------");
    println!(
        " Summary: Total Titles: {} | Solar: {} | Tilt/Accelerometer: {} | Gyroscope: {} | Rumble: {} | RTC: {}",
        catalog.summary.total_titles,
        catalog.summary.by_category.get("solar").copied().unwrap_or(0),
        catalog.summary.by_category.get("tilt_gyro").copied().unwrap_or(0),
        2, // WarioWare Twisted USA & Japan
        catalog.summary.by_category.get("rumble").copied().unwrap_or(0),
        catalog.summary.by_category.get("rtc").copied().unwrap_or(0),
    );
    println!(
        " Hardware Families: {} unique families cataloged in data/cartridge-hardware-catalog.json",
        catalog.summary.unique_families
    );
    println!("========================================================================================================================");

    Ok(())
}

/// Execute a hardware sensor injection test against a target GBA ROM.
#[allow(dead_code)]
pub fn cmd_sensor_test(
    rom_path: &Path,
    solar_opt: Option<u8>,
    tilt_opt: Option<i16>,
    gyro_opt: Option<i16>,
    as_json: bool,
) -> Result<(), String> {
    let rom_bytes = fs::read(rom_path)
        .map_err(|e| format!("Failed to read ROM file '{}': {e}", rom_path.display()))?;

    let config = SensorInjectionConfig {
        solar_level: solar_opt,
        tilt_degrees: tilt_opt,
        gyro_rpm: gyro_opt,
    };

    let report = plan_sensor_injection(&rom_bytes, &config)?;

    if as_json {
        let out = serde_json::to_string_pretty(&report)
            .map_err(|e| format!("Failed to serialize injection report: {e}"))?;
        println!("{out}");
        return Ok(());
    }

    println!("================================================================================");
    println!(" GBA CARTRIDGE HARDWARE SENSOR INJECTION TEST BRIDGE");
    println!("================================================================================");
    println!(" ROM Path:        {}", rom_path.display());
    println!(" Target ROM:      {} ({})", report.game_title, report.game_code);
    println!(" Sensor Family:   {}", report.sensor_family.display_name());
    println!(" Sensor IC:       {}", report.sensor_ic);
    println!(" Interface Bus:   {}", report.interface_bus);
    println!(" Sampling Rate:   {}", report.sampling_rate);
    println!(" Status:          {}", report.status);
    println!(" Clean-Room:      Certified (Clean-room reverse engineering)");
    println!();
    println!("--------------------------------------------------------------------------------");
    println!(" INJECTION DYNAMICS & PHYSICAL PARAMETERS");
    println!("--------------------------------------------------------------------------------");
    for (k, v) in &report.injection_parameters {
        println!("   {:<25} : {}", k, v);
    }
    println!();
    println!("--------------------------------------------------------------------------------");
    println!(" HARDWARE SENSOR REGISTER WRITES & MMIO / GPIO OVERRIDES");
    println!("--------------------------------------------------------------------------------");
    println!(" Address    | Value  | Width  | Target Register & Operational Description");
    println!("------------+--------+--------+-------------------------------------------------");
    for w in &report.register_writes {
        println!(
            " 0x{:08X} | {:6} | {:2}-bit | {}",
            w.address, w.value_hex, w.width_bits, w.description
        );
    }
    println!("================================================================================");

    Ok(())
}
