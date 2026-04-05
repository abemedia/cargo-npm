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

/// Parses a Rust target triple into an npm [`Platform`].
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

/// Resolves target triples to [`Platform`]s.
///
/// Returns the recognised platforms and any unrecognised triples.
/// Returns an error if two triples map to the same npm platform.
pub fn resolve_platforms(
    targets: impl IntoIterator<Item = impl AsRef<str>>,
) -> Result<(Vec<Platform>, Vec<String>)> {
    let mut platforms: Vec<Platform> = Vec::new();
    let mut unrecognised: Vec<String> = Vec::new();
    for t in targets {
        let t = t.as_ref();
        match parse_triple(t) {
            Some(p) => platforms.push(p),
            None => unrecognised.push(t.to_owned()),
        }
    }
    check_collisions(&platforms)?;
    normalise_libc(&mut platforms);
    Ok((platforms, unrecognised))
}

/// Strips `libc` from musl platforms that have no glibc counterpart for the same `(os, cpu)`.
///
/// After this call, `libc == Some(Libc::Musl)` means the platform is one of a dual-libc pair.
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

/// Errors if two platforms would produce the same npm package name.
/// This catches collisions like x86_64-pc-windows-msvc vs x86_64-pc-windows-gnu.
fn check_collisions(platforms: &[Platform]) -> Result<()> {
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
