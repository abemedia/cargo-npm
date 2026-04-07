use std::collections::HashMap;
use std::sync::LazyLock;

use anyhow::{Result, bail};
use strum::{Display, EnumString};

/// npm `os` field value; matches `process.platform`.
#[derive(Clone, Debug, PartialEq, Display, EnumString)]
#[strum(serialize_all = "lowercase")]
#[allow(clippy::enum_variant_names)]
pub enum Os {
    Linux,
    Darwin,
    Win32,
    FreeBsd,
    OpenBsd,
    NetBsd,
    SunOs,
    Aix,
}

/// npm `cpu` field value; matches `process.arch`.
#[derive(Clone, Debug, PartialEq, Display, EnumString)]
#[strum(serialize_all = "lowercase")]
pub enum Cpu {
    X64,
    Arm64,
    Ia32,
    Arm,
    RiscV64,
    S390x,
    Ppc64,
}

/// Linux C library variant.
#[derive(Clone, Debug, PartialEq, Display, EnumString)]
#[strum(serialize_all = "lowercase")]
pub enum Libc {
    Musl,
    Glibc,
}

/// npm-mapped platform identity for a compiled Rust target triple.
#[derive(Clone, Debug)]
pub struct Platform {
    /// Full Rust target triple (e.g. `x86_64-unknown-linux-gnu`).
    pub triple: String,
    pub os: Os,
    pub cpu: Cpu,
    pub libc: Option<Libc>,
}

/// Platform for the compiled target, detected at compile time.
pub static HOST_PLATFORM: LazyLock<Option<Platform>> =
    LazyLock::new(|| parse_triple(env!("TARGET_TRIPLE")));

/// Parse a Rust target triple into a Platform representing npm-aligned OS, CPU, and optional libc.
///
/// Accepts triples like `"x86_64-unknown-linux-gnu"` and maps the architecture, OS, and environment
/// segments to `Cpu`, `Os`, and `Option<Libc>`. Unknown architectures or OS names result in `None`.
/// For Linux targets, an environment starting with `"musl"` yields `Libc::Musl`, starting with
/// `"gnu"` yields `Libc::Glibc`; a Linux triple with any other environment returns `None`.
///
/// # Returns
///
/// `Some(Platform)` if the triple was successfully mapped; `None` if the architecture or OS is
/// unsupported, or if a Linux triple's libc environment is unrecognized.
///
/// # Examples
///
/// ```
/// let p = parse_triple("x86_64-unknown-linux-gnu").unwrap();
/// assert_eq!(p.cpu.to_string(), "x64");
/// assert_eq!(p.os.to_string(), "linux");
/// assert_eq!(p.libc.unwrap().to_string(), "glibc");
/// ```
pub fn parse_triple(triple: &str) -> Option<Platform> {
    let parts: Vec<&str> = triple.splitn(4, '-').collect();
    let arch = parts.first()?;
    let os_str = parts.get(2)?;
    let env = parts.get(3).copied().unwrap_or("");

    let cpu = match *arch {
        "x86_64" => Cpu::X64,
        "aarch64" => Cpu::Arm64,
        "i686" => Cpu::Ia32,
        "armv7" | "arm" => Cpu::Arm,
        "riscv64gc" => Cpu::RiscV64,
        "s390x" => Cpu::S390x,
        "powerpc64le" => Cpu::Ppc64,
        _ => return None,
    };

    let os = match *os_str {
        "linux" => Os::Linux,
        "darwin" => Os::Darwin,
        "windows" => Os::Win32,
        "freebsd" => Os::FreeBsd,
        "openbsd" => Os::OpenBsd,
        "netbsd" => Os::NetBsd,
        "solaris" | "illumos" => Os::SunOs,
        "aix" => Os::Aix,
        _ => return None,
    };

    let libc = match os {
        Os::Linux if env.starts_with("musl") => Some(Libc::Musl),
        Os::Linux if env.starts_with("gnu") => Some(Libc::Glibc),
        Os::Linux => return None,
        _ => None,
    };

    Some(Platform {
        triple: triple.to_string(),
        os,
        cpu,
        libc,
    })
}

/// Strips `libc` from `musl` platforms that do not have a matching `glibc` platform with the same `(os, cpu)`.
///
/// After this call, `libc == Some(Libc::Musl)` indicates the platform is part of a dual-libc pair; standalone musl entries will have `libc == None`.
///
/// # Examples
///
/// ```
/// let mut platforms: Vec<_> = [
///     "x86_64-unknown-linux-gnu",
///     "x86_64-unknown-linux-musl",
///     "aarch64-unknown-linux-musl",
/// ]
/// .iter()
/// .filter_map(|t| parse_triple(t))
/// .collect();
///
/// normalise_libc(&mut platforms);
///
/// // x86_64 musl remains because a glibc counterpart exists
/// let x86_musl = platforms.iter().find(|p| p.triple.contains("x86_64-unknown-linux-musl")).unwrap();
/// assert_eq!(x86_musl.libc, Some(Libc::Musl));
///
/// // aarch64 musl is stripped because no aarch64 glibc counterpart exists
/// let aarch_musl = platforms.iter().find(|p| p.triple.contains("aarch64-unknown-linux-musl")).unwrap();
/// assert_eq!(aarch_musl.libc, None);
/// ```
pub fn normalise_libc(platforms: &mut [Platform]) {
    let dual: Vec<bool> = platforms
        .iter()
        .map(|p| {
            p.libc == Some(Libc::Musl)
                && platforms
                    .iter()
                    .any(|q| q.os == p.os && q.cpu == p.cpu && q.libc == Some(Libc::Glibc))
        })
        .collect();
    for (p, is_dual) in platforms.iter_mut().zip(dual) {
        if p.libc == Some(Libc::Musl) && !is_dual {
            p.libc = None;
        }
    }
}

/// Detects when multiple Rust target triples would map to the same npm platform and returns an error for the first collision.
///
/// The npm platform key is formed as `"<os>-<cpu>-<libc>"`, where the libc component is empty when not present; if two different triples produce the same key, this function returns an error describing the colliding triples and the npm key.
///
/// # Examples
///
/// ```
/// # use crate::platform::{Platform, Os, Cpu, Libc, check_collisions};
/// let p1 = Platform { triple: "x86_64-pc-windows-msvc".into(), os: Os::Win32, cpu: Cpu::X64, libc: None };
/// let p2 = Platform { triple: "x86_64-pc-windows-gnu".into(), os: Os::Win32, cpu: Cpu::X64, libc: None };
/// assert!(check_collisions(&[p1, p2]).is_err());
/// ```
pub fn check_collisions(platforms: &[Platform]) -> Result<()> {
    let mut seen: HashMap<String, &str> = HashMap::new();
    for platform in platforms {
        let key = format!(
            "{}-{}-{}",
            platform.os,
            platform.cpu,
            platform
                .libc
                .as_ref()
                .map(Libc::to_string)
                .unwrap_or_default(),
        );
        if let Some(existing) = seen.get(&key) {
            bail!(
                "platform collision: triples '{}' and '{}' both map to the same npm platform ({}) - \
                 configure only one of them as a target",
                existing,
                platform.triple,
                key.trim_end_matches('-'),
            );
        }
        seen.insert(key, &platform.triple);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_collisions_detects_duplicate() {
        let platforms = vec![
            parse_triple("x86_64-pc-windows-msvc").unwrap(),
            parse_triple("x86_64-pc-windows-gnu").unwrap(),
        ];
        assert!(check_collisions(&platforms).is_err());
    }

    #[test]
    fn check_collisions_allows_distinct() {
        let platforms = vec![
            parse_triple("x86_64-unknown-linux-gnu").unwrap(),
            parse_triple("aarch64-apple-darwin").unwrap(),
        ];
        assert!(check_collisions(&platforms).is_ok());
    }

    #[test]
    fn normalise_libc_strips_standalone_musl() {
        let mut platforms = vec![parse_triple("x86_64-unknown-linux-musl").unwrap()];
        normalise_libc(&mut platforms);
        assert_eq!(platforms[0].libc, None);
    }

    #[test]
    fn normalise_libc_preserves_dual_libc_pair() {
        let mut platforms = vec![
            parse_triple("x86_64-unknown-linux-gnu").unwrap(),
            parse_triple("x86_64-unknown-linux-musl").unwrap(),
        ];
        normalise_libc(&mut platforms);
        assert_eq!(platforms[0].libc, Some(Libc::Glibc));
        assert_eq!(platforms[1].libc, Some(Libc::Musl));
    }

    #[test]
    fn normalise_libc_strips_musl_when_glibc_is_different_arch() {
        let mut platforms = vec![
            parse_triple("x86_64-unknown-linux-musl").unwrap(),
            parse_triple("aarch64-unknown-linux-gnu").unwrap(),
        ];
        normalise_libc(&mut platforms);
        assert_eq!(platforms[0].libc, None);
        assert_eq!(platforms[1].libc, Some(Libc::Glibc));
    }
}
