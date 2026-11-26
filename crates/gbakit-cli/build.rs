#![deny(unsafe_code)]

use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;

fn git_output(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn parse_formatkit_version() -> String {
    let workspace_toml = Path::new("../../Cargo.toml");
    if let Ok(contents) = fs::read_to_string(workspace_toml) {
        for line in contents.lines() {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("formatkit-core") {
                if let Some(eq_idx) = rest.find('=') {
                    let rhs = rest[eq_idx + 1..].trim();
                    if let Some(start) = rhs.find('"') {
                        let after = &rhs[start + 1..];
                        if let Some(end) = after.find('"') {
                            let ver = after[..end].trim_start_matches(['=', '^', '~', '>', '<', ' ']);
                            if !ver.is_empty() {
                                return ver.to_string();
                            }
                        }
                    }
                }
            }
        }
    }
    "0.1.17".to_string()
}

fn main() {
    println!("cargo:rerun-if-changed=../../Cargo.toml");
    if Path::new("../../.git/HEAD").exists() {
        println!("cargo:rerun-if-changed=../../.git/HEAD");
    }

    let git_commit = git_output(&["rev-parse", "--short=16", "HEAD"]).unwrap_or_default();
    let git_dirty = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .map(|o| o.status.success() && !o.stdout.is_empty())
        .unwrap_or(false);

    let target = env::var("TARGET").unwrap_or_else(|_| "unknown".to_string());
    let profile = env::var("PROFILE").unwrap_or_else(|_| "unknown".to_string());
    let formatkit_version = parse_formatkit_version();

    println!("cargo:rustc-env=GBAKIT_GIT_COMMIT={git_commit}");
    println!("cargo:rustc-env=GBAKIT_GIT_DIRTY={git_dirty}");
    println!("cargo:rustc-env=GBAKIT_TARGET={target}");
    println!("cargo:rustc-env=GBAKIT_PROFILE={profile}");
    println!("cargo:rustc-env=GBAKIT_FORMATKIT_VERSION={formatkit_version}");
}
