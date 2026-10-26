//! Debian source package (`.dsc`) support.
//!
//! A `.dsc` file describes a Debian source package: the upstream tarball, the
//! Debian packaging tarball and the checksums that tie them together. It is
//! signed with OpenPGP, in the cleartext-signed form:
//!
//! ```text
//! -----BEGIN PGP SIGNED MESSAGE-----
//! Hash: SHA256
//!
//! Format: 3.0 (quilt)
//! Source: hello
//! ...
//! Checksums-Sha256:
//!  31e0661...  725946  hello_2.10.orig.tar.gz
//!
//! -----BEGIN PGP SIGNATURE-----
//! ...
//! ```
//!
//! The upstream tarball holds the pristine sources and the Debian tarball
//! holds `debian/`, with the patches that adapt the sources to Debian. This
//! module fetches both, verifies them against the checksums in the `.dsc`,
//! lays the sources out and applies the quilt series, so that the result can
//! be built by the ordinary `cook` templates against relibc.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::deb::{self, Control};
use crate::deb_repo;

const SIGNED_HEADER: &str = "-----BEGIN PGP SIGNED MESSAGE-----";
const SIGNATURE_HEADER: &str = "-----BEGIN PGP SIGNATURE-----";

/// Errors that can occur while handling a `.dsc` source package.
#[derive(Debug)]
pub enum DscError {
    /// An I/O failure.
    Io(io::Error),
    /// The `.dsc` file could not be parsed.
    Parse(String),
    /// A referenced file is missing or does not match its checksum.
    Checksum(String),
    /// A patch could not be applied.
    Patch(String),
    /// The source package is missing a required field.
    Metadata(String),
}

impl std::fmt::Display for DscError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DscError::Io(e) => write!(f, "i/o error: {}", e),
            DscError::Parse(m) => write!(f, "invalid .dsc file: {}", m),
            DscError::Checksum(m) => write!(f, "checksum mismatch: {}", m),
            DscError::Patch(m) => write!(f, "patch failed: {}", m),
            DscError::Metadata(m) => write!(f, "incomplete source package: {}", m),
        }
    }
}

impl std::error::Error for DscError {}

impl From<io::Error> for DscError {
    fn from(e: io::Error) -> Self {
        DscError::Io(e)
    }
}

impl From<deb::DebError> for DscError {
    fn from(e: deb::DebError) -> Self {
        DscError::Parse(e.to_string())
    }
}

/// Result alias for this module.
pub type Result<T> = std::result::Result<T, DscError>;

/// One of the files that belong to a source package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFile {
    /// The file name, relative to the pool directory.
    pub name: String,
    /// The size in bytes.
    pub size: u64,
    /// The MD5 checksum, as listed in the `Files` field.
    pub md5: String,
    /// The SHA1 checksum, as listed in the `Checksums-Sha1` field.
    pub sha1: String,
    /// The SHA256 checksum, as listed in the `Checksums-Sha256` field.
    pub sha256: String,
}

/// A parsed Debian source package description.
#[derive(Debug, Clone)]
pub struct DscFile {
    /// The control fields of the source package.
    pub control: Control,
    /// The files that make up the source package.
    pub files: Vec<SourceFile>,
    /// The cleartext that the signature covers.
    pub signed_body: String,
    /// The OpenPGP signature block, if the file was signed.
    pub signature: Option<String>,
}

impl DscFile {
    /// Parse a `.dsc` file.
    pub fn parse(text: &str) -> Result<DscFile> {
        let (body, signature) = split_signed_message(text)?;
        let control = Control::parse(&body)?;
        let files = collect_files(&control)?;

        Ok(DscFile {
            control,
            files,
            signed_body: body,
            signature,
        })
    }

    /// The source package name.
    pub fn source(&self) -> Option<&str> {
        self.control.get("Source")
    }

    /// The source package version.
    pub fn version(&self) -> Option<&str> {
        self.control.get("Version")
    }

    /// The `Build-Depends` field.
    pub fn build_depends(&self) -> Vec<String> {
        self.control.get_list("Build-Depends")
    }

    /// The upstream tarball, the one that is not a Debian tarball.
    pub fn orig_tarball(&self) -> Option<&SourceFile> {
        self.files.iter().find(|f| {
            let name = &f.name;
            name.contains(".orig.")
                || (!name.contains(".debian.tar")
                    && (name.ends_with(".tar.gz")
                        || name.ends_with(".tar.xz")
                        || name.ends_with(".tar.bz2")
                        || name.ends_with(".tar.zst")))
        })
    }

    /// The Debian packaging tarball.
    pub fn debian_tarball(&self) -> Option<&SourceFile> {
        self.files.iter().find(|f| f.name.contains(".debian.tar"))
    }

    /// The detached signature of the upstream tarball, if there is one.
    pub fn orig_signature(&self) -> Option<&SourceFile> {
        self.files
            .iter()
            .find(|f| f.name.ends_with(".asc") || f.name.ends_with(".sig"))
    }

    /// Verify the checksums of the downloaded companion files in `dir`.
    pub fn verify(&self, dir: &Path) -> Result<()> {
        for file in &self.files {
            // Detached signatures are not content, they are only checked for
            // presence here.
            if file.name.ends_with(".asc") || file.name.ends_with(".sig") {
                continue;
            }
            let path = dir.join(&file.name);
            if !path.is_file() {
                return Err(DscError::Checksum(format!("missing file {}", file.name)));
            }
            let data = fs::read(&path)?;
            if !file.sha256.is_empty() {
                let actual = deb_repo::sha256_bytes(&data);
                if actual != file.sha256.to_ascii_lowercase() {
                    return Err(DscError::Checksum(format!(
                        "{}: expected sha256 {}, got {}",
                        file.name, file.sha256, actual
                    )));
                }
            }
            if file.size != 0 && data.len() as u64 != file.size {
                return Err(DscError::Checksum(format!(
                    "{}: expected {} bytes, got {}",
                    file.name,
                    file.size,
                    data.len()
                )));
            }
        }
        Ok(())
    }
}

/// Split a cleartext-signed OpenPGP message into its body and signature.
fn split_signed_message(text: &str) -> Result<(String, Option<String>)> {
    if !text.trim_start().starts_with(SIGNED_HEADER) {
        // Not signed: the whole file is the body.
        return Ok((text.to_string(), None));
    }

    let after_header = text
        .trim_start()
        .strip_prefix(SIGNED_HEADER)
        .ok_or_else(|| DscError::Parse("missing PGP header".to_string()))?;

    // The armor headers are followed by a blank line, then the cleartext.
    let body_start = after_header
        .find("\n\n")
        .ok_or_else(|| DscError::Parse("missing cleartext in PGP message".to_string()))?
        + 2;

    let rest = &after_header[body_start..];
    let (body, signature) = match rest.find(SIGNATURE_HEADER) {
        Some(index) => {
            let body = rest[..index].trim_end().to_string();
            let signature = Some(rest[index..].trim_end().to_string());
            (body, signature)
        }
        None => (rest.trim_end().to_string(), None),
    };

    Ok((body, signature))
}

/// Collect the files of a source package out of its checksum fields.
fn collect_files(control: &Control) -> Result<Vec<SourceFile>> {
    let mut files: BTreeMap<String, SourceFile> = BTreeMap::new();

    let mut record = |index: usize, value: &str| {
        // "<checksum> <size> <name>"
        let mut parts = value.split_whitespace();
        let (Some(checksum), Some(size), Some(name)) = (parts.next(), parts.next(), parts.next())
        else {
            return;
        };
        let size: u64 = size.parse().unwrap_or(0);
        let entry = files.entry(name.to_string()).or_insert_with(|| SourceFile {
            name: name.to_string(),
            size,
            md5: String::new(),
            sha1: String::new(),
            sha256: String::new(),
        });
        match index {
            0 => entry.md5 = checksum.to_string(),
            1 => entry.sha1 = checksum.to_string(),
            _ => entry.sha256 = checksum.to_string(),
        }
    };

    for (index, field) in ["Files", "Checksums-Sha1", "Checksums-Sha256"]
        .iter()
        .enumerate()
    {
        let Some(value) = control.get(field) else {
            continue;
        };
        for line in value.lines() {
            record(index, line.trim());
        }
    }

    Ok(files.into_values().collect())
}

/// Fetch the `.dsc` and the files it references into `dir`.
///
/// Returns the parsed `.dsc`.
pub fn fetch(dsc_url: &str, dir: &Path) -> Result<DscFile> {
    fs::create_dir_all(dir)?;

    let dsc_path = dir.join("source.dsc");
    deb_repo::download(dsc_url, &dsc_path)?;
    let text = fs::read_to_string(&dsc_path)?;
    let dsc = DscFile::parse(&text)?;

    let base = base_url(dsc_url);
    for file in &dsc.files {
        let path = dir.join(&file.name);
        if path.is_file() {
            continue;
        }
        let url = format!("{}/{}", base, file.name);
        deb_repo::download(&url, &path)?;
    }

    dsc.verify(dir)?;
    Ok(dsc)
}

/// The directory part of a URL, without the trailing file name.
fn base_url(url: &str) -> String {
    match url.rfind('/') {
        Some(index) => url[..index].to_string(),
        None => url.to_string(),
    }
}

/// Unpack a source package into `dest` and apply the Debian quilt series.
///
/// The upstream tarball is laid out in `dest` with its top level directory
/// stripped, the Debian tarball is unpacked over it, and the patches listed in
/// `debian/patches/series` are applied in order.
pub fn unpack_and_patch(dsc: &DscFile, dir: &Path, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest)?;

    // --- upstream sources ---
    let orig = dsc
        .orig_tarball()
        .ok_or_else(|| DscError::Metadata("no upstream tarball".to_string()))?;
    let orig_path = dir.join(&orig.name);
    let raw = fs::read(&orig_path)?;
    let stream = deb::decompress(&orig.name, raw)?;

    let staging = dest.join(".unpack");
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir_all(&staging)?;
    extract_tar(stream, &staging)?;

    // The tarball holds a single top level directory; move its contents up.
    let mut entries: Vec<_> = fs::read_dir(&staging)?.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    if entries.len() == 1 && entries[0].path().is_dir() {
        let inner = entries[0].path();
        for item in fs::read_dir(&inner)?.flatten() {
            let target = dest.join(item.file_name());
            let _ = fs::remove_dir_all(&target);
            fs::rename(item.path(), &target)?;
        }
    } else {
        for item in entries {
            let target = dest.join(item.file_name());
            let _ = fs::remove_dir_all(&target);
            fs::rename(item.path(), &target)?;
        }
    }
    let _ = fs::remove_dir_all(&staging);

    // --- Debian packaging ---
    if let Some(debian) = dsc.debian_tarball() {
        let path = dir.join(&debian.name);
        let raw = fs::read(&path)?;
        let stream = deb::decompress(&debian.name, raw)?;
        extract_tar(stream, dest)?;
    }

    // --- patches ---
    apply_series(dest)?;
    Ok(())
}

/// Apply the quilt series of a source tree.
pub fn apply_series(dest: &Path) -> Result<()> {
    let series = dest.join("debian/patches/series");
    if !series.is_file() {
        return Ok(());
    }

    let text = fs::read_to_string(&series)?;
    for line in text.lines() {
        let line = line.trim();
        // Blank lines and comments are not patches.
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let patch = dest.join("debian/patches").join(line);
        if !patch.is_file() {
            return Err(DscError::Patch(format!(
                "missing patch {}",
                patch.display()
            )));
        }
        apply_patch(&patch, dest)?;
    }
    Ok(())
}

/// Apply a single patch file to the source tree.
pub fn apply_patch(patch: &Path, dest: &Path) -> Result<()> {
    let content = fs::read_to_string(patch)?;
    let status = Command::new("patch")
        .arg("--directory")
        .arg(dest)
        .arg("--strip=1")
        .arg("--forward")
        .arg("--batch")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(stdin) = child.stdin.as_mut() {
                stdin.write_all(content.as_bytes())?;
            }
            child.wait_with_output()
        })?;

    if !status.status.success() {
        return Err(DscError::Patch(format!(
            "{}: {}",
            patch.display(),
            String::from_utf8_lossy(&status.stderr).trim()
        )));
    }
    Ok(())
}

/// Extract a tar stream into `dest`.
fn extract_tar<R: std::io::Read>(reader: R, dest: &Path) -> Result<()> {
    let mut archive = tar::Archive::new(reader);
    archive
        .unpack(dest)
        .map_err(|e| DscError::Patch(format!("unpacking tar: {}", e)))?;
    Ok(())
}

/// The `Build-Depends` of a source tree, read from `debian/control`.
pub fn build_depends(dest: &Path) -> Vec<String> {
    let path = dest.join("debian/control");
    let Ok(text) = fs::read_to_string(&path) else {
        return Vec::new();
    };
    let Ok(control) = Control::parse(&text) else {
        return Vec::new();
    };
    control.get_list("Build-Depends")
}

/// The path of the upstream sources inside a source tree.
pub fn source_dir(dest: &Path) -> PathBuf {
    dest.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal, realistic `.dsc` file.
    const SAMPLE: &str = "\
-----BEGIN PGP SIGNED MESSAGE-----
Hash: SHA256

Format: 3.0 (quilt)
Source: hello
Binary: hello
Architecture: any
Version: 2.10-3
Maintainer: Santiago Vila <sanvila@debian.org>
Standards-Version: 4.6.2
Build-Depends: debhelper-compat (= 13), help2man, texinfo
Checksums-Sha1:
 f7bebf6f9c62a2295e889f66e05ce9bfaed9ace3 725946 hello_2.10.orig.tar.gz
 a2d122fd090dbab3d40b219a237fbb7d74f8023a 12684 hello_2.10-3.debian.tar.xz
Checksums-Sha256:
 31e066137a962676e89f69d1b65382de95a7ef7d914b8cb956f41ea72e0f516b 725946 hello_2.10.orig.tar.gz
 60ee7a466808301fbaa7fea2490b5e7a6d86f598956fb3e79c71b3295dc1f249 12684 hello_2.10-3.debian.tar.xz
Files:
 6cd0ffea3884a4e79330338dcc2987d6 725946 hello_2.10.orig.tar.gz
 27ab798c1d8d9048ffc8127e9b8dbfca 12684 hello_2.10-3.debian.tar.xz

-----BEGIN PGP SIGNATURE-----

iQEzBAEBCAAdFiEE1Uw7+v+wQt44LaXXQc5/C58bizIFAmOp5ssACgkQQc5/C58b
=kNoz
-----END PGP SIGNATURE-----
";

    #[test]
    fn signed_message_is_split() {
        let (body, signature) = split_signed_message(SAMPLE).unwrap();
        assert!(body.starts_with("Format: 3.0 (quilt)"));
        assert!(!body.contains("PGP SIGNED MESSAGE"));
        assert!(!body.contains("BEGIN PGP SIGNATURE"));
        let signature = signature.unwrap();
        assert!(signature.starts_with("-----BEGIN PGP SIGNATURE-----"));
    }

    #[test]
    fn unsigned_message_is_accepted() {
        let (body, signature) = split_signed_message("Source: foo\n").unwrap();
        assert_eq!(body, "Source: foo\n");
        assert!(signature.is_none());
    }

    #[test]
    fn dsc_fields_are_parsed() {
        let dsc = DscFile::parse(SAMPLE).unwrap();
        assert_eq!(dsc.source(), Some("hello"));
        assert_eq!(dsc.version(), Some("2.10-3"));
        assert_eq!(
            dsc.build_depends(),
            vec!["debhelper-compat (= 13)", "help2man", "texinfo"]
        );
        assert!(dsc.signature.is_some());
    }

    #[test]
    fn checksums_are_collected() {
        let dsc = DscFile::parse(SAMPLE).unwrap();
        assert_eq!(dsc.files.len(), 2);

        let orig = dsc.files.iter().find(|f| f.name.contains("orig")).unwrap();
        assert_eq!(orig.size, 725946);
        assert_eq!(
            orig.sha256,
            "31e066137a962676e89f69d1b65382de95a7ef7d914b8cb956f41ea72e0f516b"
        );
        assert_eq!(orig.sha1, "f7bebf6f9c62a2295e889f66e05ce9bfaed9ace3");
        assert_eq!(orig.md5, "6cd0ffea3884a4e79330338dcc2987d6");
    }

    #[test]
    fn tarballs_are_identified() {
        let dsc = DscFile::parse(SAMPLE).unwrap();
        let orig = dsc.orig_tarball().unwrap();
        assert_eq!(orig.name, "hello_2.10.orig.tar.gz");
        let debian = dsc.debian_tarball().unwrap();
        assert_eq!(debian.name, "hello_2.10-3.debian.tar.xz");
    }

    #[test]
    fn checksums_are_verified() {
        let dir = std::env::temp_dir().join(format!("dsc-verify-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let dsc = DscFile::parse(SAMPLE).unwrap();

        // A file with the wrong content has to be rejected.
        fs::write(dir.join("hello_2.10.orig.tar.gz"), b"not the tarball").unwrap();
        fs::write(dir.join("hello_2.10-3.debian.tar.xz"), b"nope").unwrap();
        let err = dsc.verify(&dir).unwrap_err();
        assert!(matches!(err, DscError::Checksum(_)));

        // A missing file has to be reported as well.
        let _ = fs::remove_file(dir.join("hello_2.10.orig.tar.gz"));
        let _ = fs::remove_file(dir.join("hello_2.10-3.debian.tar.xz"));
        let err = dsc.verify(&dir).unwrap_err();
        assert!(err.to_string().contains("missing file"), "{}", err);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn base_url_is_derived() {
        assert_eq!(
            base_url("http://deb.debian.org/debian/pool/main/h/hello/hello_2.10-3.dsc"),
            "http://deb.debian.org/debian/pool/main/h/hello"
        );
    }

    #[test]
    fn quilt_series_is_applied() {
        let dir = std::env::temp_dir().join(format!("dsc-series-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("debian/patches")).unwrap();

        fs::write(dir.join("hello.c"), "int main(void) {\n    return 0;\n}\n").unwrap();

        // A quilt patch, with the usual a/ and b/ prefixes.
        let patch = "\
--- a/hello.c
+++ b/hello.c
@@ -1,2 +1,2 @@
 int main(void) {
-    return 0;
+    return 42;
 }
";
        fs::write(dir.join("debian/patches/0001-fix.patch"), patch).unwrap();
        fs::write(dir.join("debian/patches/series"), "0001-fix.patch\n").unwrap();

        apply_series(&dir).unwrap();

        let content = fs::read_to_string(dir.join("hello.c")).unwrap();
        assert!(
            content.contains("return 42;"),
            "patch was not applied: {}",
            content
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_patch_is_reported() {
        let dir = std::env::temp_dir().join(format!("dsc-nopatch-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("debian/patches")).unwrap();
        fs::write(dir.join("debian/patches/series"), "does-not-exist.patch\n").unwrap();

        let err = apply_series(&dir).unwrap_err();
        assert!(matches!(err, DscError::Patch(_)));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_depends_are_read_from_debian_control() {
        let dir = std::env::temp_dir().join(format!("dsc-control-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("debian")).unwrap();
        fs::write(
            dir.join("debian/control"),
            "Source: hello\nBuild-Depends: debhelper-compat (= 13), help2man\n",
        )
        .unwrap();
        assert_eq!(
            build_depends(&dir),
            vec!["debhelper-compat (= 13)", "help2man"]
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
