//! Debian repository support: `sources.list`, the `Packages` index and
//! downloading `.deb` files.
//!
//! The configuration follows Debian's own conventions, so an existing
//! `sources.list` works as-is:
//!
//! ```text
//! deb http://deb.debian.org/debian bookworm main
//! ```
//!
//! Downloads go through `wget` or `curl`, the same way the build system
//! fetches its own sources, so no TLS stack has to be linked in.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::deb::{Control, DebError};

/// Result alias for this module.
pub type Result<T> = std::result::Result<T, DebError>;

/// One `deb` line of a `sources.list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// Base URL of the archive, without a trailing slash.
    pub url: String,
    /// The distribution suite, e.g. `bookworm`.
    pub suite: String,
    /// The components to enable, e.g. `main`.
    pub components: Vec<String>,
    /// The architecture to fetch packages for.
    pub arch: String,
}

impl Source {
    /// The URL of the `Packages` index for this source and architecture.
    pub fn index_url(&self) -> String {
        let component = self
            .components
            .first()
            .map(|c| c.as_str())
            .unwrap_or("main");
        format!(
            "{}/dists/{}/{}/binary-{}/Packages.gz",
            self.url.trim_end_matches('/'),
            self.suite,
            component,
            self.arch
        )
    }
}

/// Parse the `deb` lines of a `sources.list`.
///
/// Lines starting with `#` and other types (`deb-src`, `deb [arch=...]`) are
/// ignored.
pub fn parse_sources_list(text: &str) -> Vec<Source> {
    let mut sources = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        if parts.next() != Some("deb") {
            continue;
        }
        let Some(url) = parts.next() else { continue };
        let Some(suite) = parts.next() else { continue };
        let mut components: Vec<String> = parts.map(|c| c.to_string()).collect();
        if components.is_empty() {
            components.push("main".to_string());
        }
        sources.push(Source {
            url: url.to_string(),
            suite: suite.to_string(),
            components,
            arch: String::new(),
        });
    }
    sources
}

/// A package as described by the `Packages` index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebEntry {
    /// Package name.
    pub package: String,
    /// Package version.
    pub version: String,
    /// Target architecture.
    pub architecture: String,
    /// Path of the `.deb` inside the archive.
    pub filename: String,
    /// Size of the `.deb` in bytes.
    pub size: u64,
    /// The expected SHA256 checksum, lowercase hex.
    pub sha256: String,
    /// The `Depends` field.
    pub depends: Vec<String>,
}

/// The parsed contents of a `Packages` index.
#[derive(Debug, Clone, Default)]
pub struct PackagesIndex {
    entries: BTreeMap<String, DebEntry>,
}

impl PackagesIndex {
    /// Parse a `Packages` index.
    pub fn parse(text: &str) -> Result<PackagesIndex> {
        let mut index = PackagesIndex::default();
        for stanza in crate::deb_install::split_stanzas(text) {
            let control = Control::parse(&stanza)?;
            let Some(package) = control.package() else {
                continue;
            };
            let entry = DebEntry {
                package: package.to_string(),
                version: control.version().unwrap_or("").to_string(),
                architecture: control.architecture().unwrap_or("").to_string(),
                filename: control.get("Filename").unwrap_or("").to_string(),
                size: control
                    .get("Size")
                    .and_then(|s| s.trim().parse().ok())
                    .unwrap_or(0),
                sha256: control
                    .get("SHA256")
                    .unwrap_or("")
                    .trim()
                    .to_ascii_lowercase(),
                depends: control.get_list("Depends"),
            };
            index.entries.insert(entry.package.clone(), entry);
        }
        Ok(index)
    }

    /// Look a package up by name.
    pub fn get(&self, name: &str) -> Option<&DebEntry> {
        self.entries.get(name)
    }

    /// All packages in the index.
    pub fn entries(&self) -> impl Iterator<Item = &DebEntry> {
        self.entries.values()
    }

    /// Search the index for packages whose name contains `term`.
    pub fn search(&self, term: &str) -> Vec<&DebEntry> {
        let needle = term.to_lowercase();
        self.entries
            .values()
            .filter(|e| e.package.to_lowercase().contains(&needle))
            .collect()
    }

    /// Merge another index into this one, letting the other take precedence.
    pub fn merge(&mut self, other: PackagesIndex) {
        self.entries.extend(other.entries);
    }
}

/// Map a Carcinization target architecture onto the Debian name.
pub fn debian_arch(arch: &str) -> Option<&'static str> {
    match arch {
        "x86_64" => Some("amd64"),
        "aarch64" => Some("arm64"),
        "i586" | "i686" => Some("i386"),
        "riscv64gc" | "riscv64" => Some("riscv64"),
        _ => None,
    }
}

/// Download a URL to a file, using `wget` or `curl`.
pub fn download(url: &str, dest: &Path) -> Result<()> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }

    let wget = Command::new("wget")
        .arg("-q")
        .arg("-O")
        .arg(dest)
        .arg(url)
        .status();
    if let Ok(status) = wget
        && status.success()
    {
        return Ok(());
    }

    let curl = Command::new("curl")
        .arg("-fsSL")
        .arg("-o")
        .arg(dest)
        .arg(url)
        .status();
    if let Ok(status) = curl
        && status.success()
    {
        return Ok(());
    }

    Err(DebError::Io(io::Error::other(format!(
        "failed to download {} with wget or curl",
        url
    ))))
}

/// Compute the SHA256 of a file, lowercase hex.
pub fn sha256_file(path: &Path) -> Result<String> {
    // Use the system's sha256sum when available and fall back to a pure
    // implementation of SHA256 otherwise.
    let output = Command::new("sha256sum").arg(path).output();
    if let Ok(output) = output
        && output.status.success()
    {
        let text = String::from_utf8_lossy(&output.stdout);
        if let Some(hash) = text.split_whitespace().next() {
            return Ok(hash.to_ascii_lowercase());
        }
    }
    Ok(sha256_bytes(&fs::read(path)?))
}

/// Pure Rust SHA-256, used when `sha256sum` is not available.
pub fn sha256_bytes(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    let mut message = data.to_vec();
    let bit_len = (data.len() as u64) * 8;
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in message.chunks(64) {
        let mut w = [0u32; 64];
        for (i, word) in chunk.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    h.iter().map(|word| format!("{:08x}", word)).collect()
}

/// Verify a downloaded file against its expected SHA256.
pub fn verify_sha256(path: &Path, expected: &str) -> Result<()> {
    if expected.is_empty() {
        return Ok(());
    }
    let actual = sha256_file(path)?;
    if actual != expected.to_ascii_lowercase() {
        return Err(DebError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "checksum mismatch for {}: expected {}, got {}",
                path.display(),
                expected,
                actual
            ),
        )));
    }
    Ok(())
}

/// Fetch the `Packages` index of a source and return its text.
pub fn fetch_index_text(source: &Source, work_dir: &Path) -> Result<String> {
    let url = source.index_url();
    let path = work_dir.join("Packages.gz");
    download(&url, &path)?;

    let raw = fs::read(&path)?;
    if raw.starts_with(&[0x1f, 0x8b]) {
        let mut decoder = flate2::read::GzDecoder::new(raw.as_slice());
        let mut out = String::new();
        use std::io::Read;
        decoder.read_to_string(&mut out)?;
        Ok(out)
    } else {
        Ok(String::from_utf8_lossy(&raw).into_owned())
    }
}

/// Fetch and parse the `Packages` index of a source.
pub fn fetch_index(source: &Source, work_dir: &Path) -> Result<PackagesIndex> {
    PackagesIndex::parse(&fetch_index_text(source, work_dir)?)
}

/// Write text to a file, gzip compressed.
pub fn write_gzip(path: &Path, text: &str) -> Result<()> {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = fs::File::create(path)?;
    let mut encoder = GzEncoder::new(file, Compression::default());
    use std::io::Write;
    encoder.write_all(text.as_bytes())?;
    encoder.finish()?;
    Ok(())
}

/// Read a file that may or may not be gzip compressed.
pub fn read_maybe_gzip(path: &Path) -> Result<String> {
    let raw = fs::read(path)?;
    if raw.starts_with(&[0x1f, 0x8b]) {
        let mut decoder = flate2::read::GzDecoder::new(raw.as_slice());
        let mut out = String::new();
        use std::io::Read;
        decoder.read_to_string(&mut out)?;
        Ok(out)
    } else {
        Ok(String::from_utf8_lossy(&raw).into_owned())
    }
}

/// The default location of the sources list inside a root.
pub fn sources_list_path(root: &Path) -> PathBuf {
    root.join("etc/apt/sources.list")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_list_is_parsed() {
        let text = "\
# a comment
deb http://deb.debian.org/debian bookworm main
deb-src http://deb.debian.org/debian bookworm main

deb http://security.debian.org/debian-security bookworm-security main
";
        let sources = parse_sources_list(text);
        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0].url, "http://deb.debian.org/debian");
        assert_eq!(sources[0].suite, "bookworm");
        assert_eq!(sources[0].components, vec!["main"]);
        assert_eq!(sources[1].suite, "bookworm-security");
    }

    #[test]
    fn index_url_is_built_from_the_source() {
        let source = Source {
            url: "http://deb.debian.org/debian".to_string(),
            suite: "bookworm".to_string(),
            components: vec!["main".to_string()],
            arch: "amd64".to_string(),
        };
        assert_eq!(
            source.index_url(),
            "http://deb.debian.org/debian/dists/bookworm/main/binary-amd64/Packages.gz"
        );
    }

    #[test]
    fn packages_index_is_parsed() {
        let text = "\
Package: hello
Version: 2.10-3
Architecture: amd64
Depends: libc6, zlib1g
Filename: pool/main/h/hello/hello_2.10-3_amd64.deb
Size: 5416
SHA256: 0000000000000000000000000000000000000000000000000000000000000001

Package: zlib1g
Version: 1:1.2.13.dfsg-1
Architecture: amd64
Filename: pool/main/z/zlib1g/zlib1g_1.2.13_amd64.deb
Size: 100
SHA256: abc
";
        let index = PackagesIndex::parse(text).unwrap();
        assert_eq!(index.entries().count(), 2);

        let hello = index.get("hello").unwrap();
        assert_eq!(hello.version, "2.10-3");
        assert_eq!(hello.size, 5416);
        assert_eq!(hello.depends, vec!["libc6", "zlib1g"]);
        assert_eq!(hello.filename, "pool/main/h/hello/hello_2.10-3_amd64.deb");
        assert_eq!(
            hello.sha256,
            "0000000000000000000000000000000000000000000000000000000000000001"
        );
    }

    #[test]
    fn package_search_is_case_insensitive() {
        let text = "Package: libcurl4\nVersion: 1\nArchitecture: amd64\n";
        let index = PackagesIndex::parse(text).unwrap();
        assert_eq!(index.search("CURL").len(), 1);
        assert_eq!(index.search("nothing").len(), 0);
    }

    #[test]
    fn architectures_are_mapped() {
        assert_eq!(debian_arch("x86_64"), Some("amd64"));
        assert_eq!(debian_arch("aarch64"), Some("arm64"));
        assert_eq!(debian_arch("riscv64gc"), Some("riscv64"));
        assert_eq!(debian_arch("mips"), None);
    }

    #[test]
    fn sha256_matches_known_vectors() {
        assert_eq!(
            sha256_bytes(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_bytes(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_bytes(b"hello world"),
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }
}
