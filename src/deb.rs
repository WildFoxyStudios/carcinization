//! Parsing and unpacking of Debian `.deb` packages.
//!
//! A `.deb` is a Unix `ar` archive holding three members:
//!
//! - `debian-binary` — the format version, always `2.0`.
//! - `control.tar.{gz,xz,zst,bz2}` — the package metadata (`control`) and the
//!   maintainer scripts (`preinst`, `postinst`, `prerm`, `postrm`, `config`).
//! - `data.tar.{gz,xz,zst,bz2}` — the file tree that gets installed.
//!
//! Everything here is implemented on top of pure Rust crates so that it works
//! the same on the build host and inside the guest, without depending on
//! external `tar`/`xz`/`zstd` binaries.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Errors that can occur while handling a `.deb` package.
#[derive(Debug)]
pub enum DebError {
    /// An I/O failure.
    Io(io::Error),
    /// The file is not a valid `ar` archive, or is not a `.deb`.
    Ar(String),
    /// The `control` file could not be parsed.
    Control(String),
    /// The payload used a compression scheme we do not support.
    UnsupportedCompression(String),
    /// A required archive member is missing.
    MissingMember(String),
}

impl std::fmt::Display for DebError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DebError::Io(e) => write!(f, "i/o error: {}", e),
            DebError::Ar(m) => write!(f, "invalid .deb archive: {}", m),
            DebError::Control(m) => write!(f, "invalid control file: {}", m),
            DebError::UnsupportedCompression(m) => write!(f, "unsupported compression: {}", m),
            DebError::MissingMember(m) => write!(f, "missing archive member: {}", m),
        }
    }
}

impl std::error::Error for DebError {}

impl From<io::Error> for DebError {
    fn from(e: io::Error) -> Self {
        DebError::Io(e)
    }
}

/// Result alias for this module.
pub type Result<T> = std::result::Result<T, DebError>;

// ---------------------------------------------------------------------------
// ar archive
// ---------------------------------------------------------------------------

const AR_MAGIC: &[u8; 8] = b"!<arch>\n";
const AR_HEADER_LEN: u64 = 60;

/// One member of an `ar` archive.
#[derive(Debug, Clone)]
pub struct ArEntry {
    /// Member name, with the trailing `/` used by GNU `ar` removed.
    pub name: String,
    /// Offset of the member payload inside the archive.
    pub offset: u64,
    /// Size of the member payload in bytes.
    pub size: u64,
}

/// A reader over the members of a Unix `ar` archive.
#[derive(Debug)]
pub struct ArArchive<R: Read + Seek> {
    reader: R,
    entries: Vec<ArEntry>,
}

impl<R: Read + Seek> ArArchive<R> {
    /// Parse the archive index of `reader`.
    pub fn new(mut reader: R) -> Result<Self> {
        let mut magic = [0u8; 8];
        reader.read_exact(&mut magic)?;
        if &magic != AR_MAGIC {
            return Err(DebError::Ar("missing !<arch> magic".to_string()));
        }

        let mut entries = Vec::new();
        let mut pos: u64 = AR_MAGIC.len() as u64;
        loop {
            let mut header = [0u8; AR_HEADER_LEN as usize];
            match reader.read_exact(&mut header) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e.into()),
            }

            let name_field = &header[0..16];
            let name = String::from_utf8_lossy(name_field)
                .trim()
                .trim_end_matches('/')
                .to_string();
            if name.is_empty() {
                break;
            }

            let size_field = String::from_utf8_lossy(&header[48..58]);
            let size: u64 = size_field
                .trim()
                .parse()
                .map_err(|_| DebError::Ar(format!("bad member size for '{}'", name)))?;

            entries.push(ArEntry {
                name,
                offset: pos + AR_HEADER_LEN,
                size,
            });

            // Payloads are padded to an even number of bytes.
            pos += AR_HEADER_LEN + size + (size % 2);
            reader.seek(SeekFrom::Start(pos))?;
        }

        Ok(ArArchive { reader, entries })
    }

    /// All member names in the archive.
    pub fn names(&self) -> Vec<&str> {
        self.entries.iter().map(|e| e.name.as_str()).collect()
    }

    /// Find a member by name.
    pub fn find(&self, name: &str) -> Option<&ArEntry> {
        self.entries.iter().find(|e| e.name == name)
    }

    /// Find the first member whose name starts with `prefix`.
    pub fn find_prefix(&self, prefix: &str) -> Option<&ArEntry> {
        self.entries.iter().find(|e| e.name.starts_with(prefix))
    }

    /// Stream the payload of a member.
    pub fn member_reader(&mut self, entry: &ArEntry) -> io::Result<impl Read + '_> {
        self.reader.seek(SeekFrom::Start(entry.offset))?;
        Ok(self.reader.by_ref().take(entry.size))
    }

    /// Read the whole payload of a member into memory.
    pub fn member_bytes(&mut self, entry: &ArEntry) -> io::Result<Vec<u8>> {
        let mut buf = Vec::with_capacity(entry.size as usize);
        self.member_reader(entry)?.read_to_end(&mut buf)?;
        Ok(buf)
    }
}

// ---------------------------------------------------------------------------
// control file
// ---------------------------------------------------------------------------

/// A parsed Debian `control` file: an RFC822-like set of fields.
#[derive(Debug, Clone, Default)]
pub struct Control {
    fields: BTreeMap<String, String>,
}

impl Control {
    /// Parse a `control` file.
    ///
    /// Continuation lines start with a space or a tab and are appended to the
    /// value of the preceding field. This is also how Debian encodes the
    /// extended description: every line after `Description:` is indented,
    /// and a lone "." stands for an empty line.
    pub fn parse(text: &str) -> Result<Control> {
        let mut fields: BTreeMap<String, String> = BTreeMap::new();
        let mut current: Option<(String, String)> = None;

        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            if line.starts_with(' ') || line.starts_with('\t') {
                match current.as_mut() {
                    Some((_, value)) => {
                        value.push('\n');
                        value.push_str(line.trim_start());
                    }
                    None => {
                        return Err(DebError::Control(
                            "continuation line without a preceding field".to_string(),
                        ));
                    }
                }
                continue;
            }
            if let Some((key, value)) = current.take() {
                fields.insert(key, value);
            }
            let Some((key, value)) = line.split_once(':') else {
                return Err(DebError::Control(format!("malformed line: {}", line)));
            };
            current = Some((key.trim().to_string(), value.trim().to_string()));
        }
        if let Some((key, value)) = current {
            fields.insert(key, value);
        }

        Ok(Control { fields })
    }

    /// The raw value of a field.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.fields.get(key).map(|s| s.as_str())
    }

    /// A field split on commas, as used by `Depends` and friends.
    pub fn get_list(&self, key: &str) -> Vec<String> {
        self.get(key)
            .map(|value| {
                value
                    .split(',')
                    .map(|item| item.trim().to_string())
                    .filter(|item| !item.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The package name.
    pub fn package(&self) -> Option<&str> {
        self.get("Package")
    }

    /// The package version.
    pub fn version(&self) -> Option<&str> {
        self.get("Version")
    }

    /// The target architecture.
    pub fn architecture(&self) -> Option<&str> {
        self.get("Architecture")
    }

    /// Set a field, replacing any previous value.
    pub fn set(&mut self, key: &str, value: &str) {
        self.fields.insert(key.to_string(), value.to_string());
    }

    /// Render the fields back into a control stanza.
    ///
    /// Continuation lines are indented with a space, which is how Debian
    /// encodes multi-line values, so that the stanza can be read back
    /// unchanged.
    pub fn to_stanza(&self) -> String {
        let mut text = String::new();
        for (key, value) in &self.fields {
            text.push_str(key);
            text.push_str(": ");
            let mut lines = value.split('\n');
            if let Some(first) = lines.next() {
                text.push_str(first);
            }
            for line in lines {
                text.push('\n');
                text.push(' ');
                text.push_str(line);
            }
            text.push('\n');
        }
        text
    }
}

// ---------------------------------------------------------------------------
// decompression
// ---------------------------------------------------------------------------

/// Decompress a payload according to the compression suffix of `name`.
///
/// The returned reader yields the decompressed bytes.
pub fn decompress(name: &str, data: Vec<u8>) -> Result<Box<dyn Read>> {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".gz") || lower.ends_with(".gzip") {
        Ok(Box::new(flate2::read::GzDecoder::new(io::Cursor::new(
            data,
        ))))
    } else if lower.ends_with(".xz") {
        let mut cursor = io::Cursor::new(data);
        let mut out = Vec::new();
        lzma_rs::xz_decompress(&mut cursor, &mut out)
            .map_err(|e| DebError::UnsupportedCompression(format!("xz: {}", e)))?;
        Ok(Box::new(io::Cursor::new(out)))
    } else if lower.ends_with(".zst") || lower.ends_with(".zstd") {
        let decoder = ruzstd::decoding::StreamingDecoder::new(io::Cursor::new(data))
            .map_err(|e| DebError::UnsupportedCompression(format!("zstd: {}", e)))?;
        Ok(Box::new(decoder))
    } else if lower.ends_with(".bz2") || lower.ends_with(".bzip2") {
        Ok(Box::new(bzip2_rs::DecoderReader::new(io::Cursor::new(
            data,
        ))))
    } else if lower.ends_with(".tar") {
        Ok(Box::new(io::Cursor::new(data)))
    } else {
        Err(DebError::UnsupportedCompression(name.to_string()))
    }
}

/// Extract a tar stream into `dest`.
///
/// The `tar` crate sets the metadata of the files it extracts with calls that
/// work on an open file descriptor. Redox only implements the variants that
/// take a path, so the extraction is done here: the contents are written and
/// the permissions are set on the path, leaving out the metadata that the
/// filesystem does not support.
fn extract_tar<R: Read>(reader: R, dest: &Path) -> Result<()> {
    let mut archive = tar::Archive::new(reader);
    // The modification times are set with an operation on a file descriptor
    // that Redox does not implement.
    archive.set_preserve_mtime(false);

    let entries = archive
        .entries()
        .map_err(|e| DebError::Ar(format!("reading the archive: {e}")))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| DebError::Ar(format!("reading the archive: {e}")))?;
        let path = entry
            .path()
            .map_err(|e| DebError::Ar(format!("invalid path in the archive: {e}")))?
            .into_owned();
        let target = join_within(dest, &path)?;
        let kind = entry.header().entry_type();

        if kind.is_dir() {
            fs::create_dir_all(&target)?;
        } else if kind.is_symlink() {
            let link = entry
                .link_name()
                .map_err(|e| DebError::Ar(format!("invalid link in the archive: {e}")))?
                .ok_or_else(|| DebError::Ar("symlink without a target".to_string()))?;
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            let _ = fs::remove_file(&target);
            std::os::unix::fs::symlink(&link, &target)?;
        } else if kind.is_hard_link() {
            let link = entry
                .link_name()
                .map_err(|e| DebError::Ar(format!("invalid link in the archive: {e}")))?
                .ok_or_else(|| DebError::Ar("hard link without a target".to_string()))?;
            let source = join_within(dest, &link)?;
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            let _ = fs::remove_file(&target);
            fs::hard_link(source, &target)?;
        } else if kind.is_file() {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut file = fs::File::create(&target)?;
            io::copy(&mut entry, &mut file)?;
            if let Ok(mode) = entry.header().mode() {
                set_mode(&target, mode);
            }
        }
    }
    Ok(())
}

/// Set the permissions of a path, ignoring the filesystems that do not
/// support it.
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o7777));
}

/// Join a path from an archive to the destination, refusing the entries that
/// would escape it.
fn join_within(dest: &Path, path: &Path) -> Result<PathBuf> {
    let mut target = dest.to_path_buf();
    for component in path.components() {
        match component {
            std::path::Component::Normal(part) => target.push(part),
            std::path::Component::CurDir => {}
            _ => {
                return Err(DebError::Ar(format!(
                    "the archive entry {} escapes the destination",
                    path.display()
                )));
            }
        }
    }
    Ok(target)
}

// ---------------------------------------------------------------------------
// package
// ---------------------------------------------------------------------------

/// The maintainer scripts carried by a package.
pub type MaintainerScripts = BTreeMap<String, String>;

/// A `.deb` package that has been unpacked to disk.
#[derive(Debug)]
pub struct DebPackage {
    /// Parsed `control` metadata.
    pub control: Control,
    /// The installed file tree, extracted from `data.tar.*`.
    pub data_dir: PathBuf,
    /// The maintainer scripts, keyed by script name.
    pub scripts: MaintainerScripts,
}

impl DebPackage {
    /// The package name.
    pub fn name(&self) -> Option<&str> {
        self.control.package()
    }

    /// The package version.
    pub fn version(&self) -> Option<&str> {
        self.control.version()
    }

    /// The `Depends` field, split into individual constraints.
    pub fn depends(&self) -> Vec<String> {
        self.control.get_list("Depends")
    }
}

/// Unpack a `.deb` file.
///
/// `work_dir` is used for the intermediate `control` extraction; the file
/// tree ends up in `work_dir/data`.
pub fn unpack(deb_path: &Path, work_dir: &Path) -> Result<DebPackage> {
    let file = File::open(deb_path)?;
    let mut archive = ArArchive::new(file)?;

    if archive.find("debian-binary").is_none() {
        return Err(DebError::Ar(
            "not a Debian package: no debian-binary member".to_string(),
        ));
    }

    // --- control.tar.* ---
    let control_entry = archive
        .find_prefix("control.tar")
        .cloned()
        .ok_or_else(|| DebError::MissingMember("control.tar.*".to_string()))?;
    let control_name = control_entry.name.clone();
    let control_bytes = archive.member_bytes(&control_entry)?;
    let control_stream = decompress(&control_name, control_bytes)?;

    let control_dir = work_dir.join("control");
    fs::create_dir_all(&control_dir)?;
    extract_tar(control_stream, &control_dir)?;

    let control_text = fs::read_to_string(control_dir.join("control"))
        .map_err(|e| DebError::Control(format!("reading control file: {}", e)))?;
    let control = Control::parse(&control_text)?;

    let mut scripts = MaintainerScripts::new();
    for name in ["preinst", "postinst", "prerm", "postrm", "config"] {
        let path = control_dir.join(name);
        if path.is_file()
            && let Ok(content) = fs::read_to_string(&path)
        {
            scripts.insert(name.to_string(), content);
        }
    }

    // --- data.tar.* ---
    let data_entry = archive
        .find_prefix("data.tar")
        .cloned()
        .ok_or_else(|| DebError::MissingMember("data.tar.*".to_string()))?;
    let data_name = data_entry.name.clone();
    let data_bytes = archive.member_bytes(&data_entry)?;
    let data_stream = decompress(&data_name, data_bytes)?;

    let data_dir = work_dir.join("data");
    fs::create_dir_all(&data_dir)?;
    extract_tar(data_stream, &data_dir)?;

    Ok(DebPackage {
        control,
        data_dir,
        scripts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Build a minimal `ar` archive in memory.
    fn build_ar(members: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(AR_MAGIC);
        for (name, data) in members {
            let mut header = [b' '; AR_HEADER_LEN as usize];
            header[..name.len()].copy_from_slice(name.as_bytes());
            let size = format!("{}", data.len());
            header[48..48 + size.len()].copy_from_slice(size.as_bytes());
            header[58] = 0x60; // '`'
            header[59] = b'\n';
            out.extend_from_slice(&header);
            out.extend_from_slice(data);
            if data.len() % 2 == 1 {
                out.push(b'\n');
            }
        }
        out
    }

    /// Build a tar archive in memory.
    fn build_tar(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (name, data) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, name, *data)
                .expect("append tar entry");
        }
        builder.into_inner().expect("finish tar")
    }

    #[test]
    fn ar_index_is_parsed() {
        let bytes = build_ar(&[("debian-binary", b"2.0\n"), ("control.tar.gz", b"payload")]);
        let archive = ArArchive::new(io::Cursor::new(bytes)).unwrap();
        assert_eq!(archive.names(), vec!["debian-binary", "control.tar.gz"]);
        let entry = archive.find("control.tar.gz").unwrap();
        assert_eq!(entry.size, 7);
    }

    #[test]
    fn ar_member_payload_is_readable() {
        let bytes = build_ar(&[("debian-binary", b"2.0\n")]);
        let mut archive = ArArchive::new(io::Cursor::new(bytes)).unwrap();
        let entry = archive.find("debian-binary").unwrap().clone();
        let payload = archive.member_bytes(&entry).unwrap();
        assert_eq!(payload, b"2.0\n");
    }

    #[test]
    fn ar_rejects_non_ar_files() {
        let err = ArArchive::new(io::Cursor::new(b"not an archive".to_vec())).unwrap_err();
        assert!(matches!(err, DebError::Ar(_)));
    }

    #[test]
    fn ar_reports_missing_magic() {
        let err = ArArchive::new(io::Cursor::new(vec![0u8; 4])).unwrap_err();
        assert!(matches!(err, DebError::Io(_) | DebError::Ar(_)));
    }

    #[test]
    fn tar_entries_are_extracted_with_their_mode() {
        let dir = std::env::temp_dir().join("carcinization-deb-extract-test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(5);
        header.set_mode(0o755);
        header.set_cksum();
        builder
            .append_data(&mut header, "usr/bin/tool", &b"hello"[..])
            .unwrap();
        let archive = builder.into_inner().unwrap();

        extract_tar(io::Cursor::new(archive), &dir).unwrap();

        let tool = dir.join("usr/bin/tool");
        assert_eq!(fs::read(&tool).unwrap(), b"hello");
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&tool).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);

        let _ = fs::remove_dir_all(&dir);
    }

    /// Build a tar entry by hand, so that the paths the builder refuses to
    /// write can be tested.
    fn build_raw_tar_entry(name: &str, data: &[u8]) -> Vec<u8> {
        let mut header = [0u8; 512];
        header[..name.len()].copy_from_slice(name.as_bytes());
        header[100..107].copy_from_slice(b"0000644");
        header[108..115].copy_from_slice(b"0000000");
        header[116..123].copy_from_slice(b"0000000");
        let size = format!("{:011o}", data.len());
        header[124..135].copy_from_slice(size.as_bytes());
        header[136..147].copy_from_slice(b"00000000000");
        header[156] = b'0';
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        for byte in header.iter_mut().skip(148).take(8) {
            *byte = b' ';
        }
        let sum: u32 = header.iter().map(|byte| *byte as u32).sum();
        let checksum = format!("{:06o}\0 ", sum);
        header[148..156].copy_from_slice(checksum.as_bytes());

        let mut out = header.to_vec();
        out.extend_from_slice(data);
        let padding = (512 - data.len() % 512) % 512;
        out.extend(std::iter::repeat_n(0u8, padding));
        out
    }

    #[test]
    fn tar_entries_cannot_escape_the_destination() {
        let dir = std::env::temp_dir().join("carcinization-deb-escape-test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let archive = build_raw_tar_entry("../escaped", b"x");

        // The extraction refuses the entry, so it never reaches the file
        // system.
        assert!(extract_tar(io::Cursor::new(archive), &dir).is_err());
        assert!(!dir.parent().unwrap().join("escaped").exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn control_parses_fields() {
        let control = Control::parse(
            "Package: hello\nVersion: 2.10-3\nArchitecture: amd64\nDepends: libc6, zlib1g\n",
        )
        .unwrap();
        assert_eq!(control.package(), Some("hello"));
        assert_eq!(control.version(), Some("2.10-3"));
        assert_eq!(control.architecture(), Some("amd64"));
        assert_eq!(control.get_list("Depends"), vec!["libc6", "zlib1g"]);
    }

    #[test]
    fn control_handles_continuation_lines() {
        let control = Control::parse("Package: hello\nDescription: short\n long line\n").unwrap();
        assert_eq!(control.get("Description"), Some("short\nlong line"));
    }

    #[test]
    fn control_rejects_malformed_lines() {
        assert!(Control::parse("this is not a field\n").is_err());
    }

    #[test]
    fn control_reads_the_debian_extended_description() {
        // The real format used by Debian packages: every line of the long
        // description is indented, and a lone "." stands for an empty line.
        let control = Control::parse(
            &[
                "Package: hello",
                "Description: example package based on GNU hello",
                " The GNU hello program produces a familiar greeting.",
                " .",
                " Seriously, though: this is an example.",
                "Maintainer: Test <t@example.com>",
            ]
            .join("\n"),
        )
        .unwrap();

        let description = control.get("Description").unwrap();
        assert!(description.starts_with("example package based on GNU hello"));
        assert!(description.contains("The GNU hello program produces a familiar greeting."));
        assert!(description.contains(".\n"));
        // The field that follows the description is still parsed.
        assert_eq!(control.get("Maintainer"), Some("Test <t@example.com>"));
    }

    #[test]
    fn control_round_trips_through_a_stanza() {
        let original = [
            "Package: hello",
            "Version: 2.10-3",
            "Description: short summary",
            " first long line",
            " second long line",
            "Maintainer: Test <t@example.com>",
        ]
        .join("\n");

        let control = Control::parse(&original).unwrap();
        let stanza = control.to_stanza();

        // The rendered stanza must parse back to exactly the same fields.
        let reparsed = Control::parse(&stanza).unwrap();
        assert_eq!(reparsed.get("Package"), control.get("Package"));
        assert_eq!(reparsed.get("Description"), control.get("Description"));
        assert_eq!(reparsed.get("Maintainer"), control.get("Maintainer"));

        // Continuation lines must keep their indentation, otherwise the
        // stanza cannot be read back.
        for line in stanza.lines() {
            if line.contains("long line") {
                assert!(line.starts_with(' '), "lost indentation: {}", line);
            }
        }
    }

    #[test]
    fn control_missing_field_is_none() {
        let control = Control::parse("Package: hello\n").unwrap();
        assert_eq!(control.get("Version"), None);
        assert!(control.get_list("Depends").is_empty());
    }

    #[test]
    fn tar_extraction_writes_files() {
        let dir = std::env::temp_dir().join(format!("deb-tar-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let tar = build_tar(&[("usr/bin/hello", b"binary")]);
        extract_tar(io::Cursor::new(tar), &dir).unwrap();
        assert_eq!(
            fs::read_to_string(dir.join("usr/bin/hello")).unwrap(),
            "binary"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn gzip_payload_is_decompressed() {
        use flate2::Compression;
        use flate2::write::GzEncoder;
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(b"hello deb").unwrap();
        let compressed = encoder.finish().unwrap();
        let mut out = String::new();
        decompress("data.tar.gz", compressed)
            .unwrap()
            .read_to_string(&mut out)
            .unwrap();
        assert_eq!(out, "hello deb");
    }

    #[test]
    fn xz_payload_is_decompressed() {
        let mut compressed = Vec::new();
        lzma_rs::xz_compress(&mut io::Cursor::new(b"hello xz".to_vec()), &mut compressed).unwrap();
        let mut out = String::new();
        decompress("data.tar.xz", compressed)
            .unwrap()
            .read_to_string(&mut out)
            .unwrap();
        assert_eq!(out, "hello xz");
    }

    #[test]
    fn zstd_payload_is_decompressed() {
        let compressed = ruzstd::encoding::compress_to_vec(
            io::Cursor::new(b"hello zstd".to_vec()),
            // ruzstd's encoder only implements Uncompressed and Fastest;
            // the decoder handles every level produced by real zstd.
            ruzstd::encoding::CompressionLevel::Fastest,
        );
        let mut out = String::new();
        decompress("data.tar.zst", compressed)
            .unwrap()
            .read_to_string(&mut out)
            .unwrap();
        assert_eq!(out, "hello zstd");
    }

    #[test]
    fn uncompressed_payload_is_passed_through() {
        let mut out = String::new();
        decompress("data.tar", b"plain".to_vec())
            .unwrap()
            .read_to_string(&mut out)
            .unwrap();
        assert_eq!(out, "plain");
    }

    #[test]
    fn unknown_compression_is_rejected() {
        match decompress("data.tar.lz4", vec![]) {
            Err(DebError::UnsupportedCompression(_)) => {}
            Err(other) => panic!("unexpected error: {}", other),
            Ok(_) => panic!("expected an error for an unknown compression"),
        }
    }

    #[test]
    fn unpack_reads_control_data_and_scripts() {
        let dir = std::env::temp_dir().join(format!("deb-unpack-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let control_tar = build_tar(&[
            (
                "control",
                b"Package: hello\nVersion: 1.0\nArchitecture: amd64\nDepends: libc6\n",
            ),
            ("postinst", b"#!/bin/sh\necho installed\n"),
        ]);
        let data_tar = build_tar(&[("usr/share/doc/hello/README", b"docs")]);

        let mut control_enc =
            flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        control_enc.write_all(&control_tar).unwrap();
        let control_gz = control_enc.finish().unwrap();

        let deb = build_ar(&[
            ("debian-binary", b"2.0\n"),
            ("control.tar.gz", &control_gz),
            ("data.tar", &data_tar),
        ]);

        let deb_path = dir.join("hello.deb");
        fs::write(&deb_path, &deb).unwrap();

        let package = unpack(&deb_path, &dir.join("work")).unwrap();
        assert_eq!(package.name(), Some("hello"));
        assert_eq!(package.version(), Some("1.0"));
        assert_eq!(package.depends(), vec!["libc6"]);
        assert!(package.scripts.contains_key("postinst"));
        assert_eq!(
            fs::read_to_string(package.data_dir.join("usr/share/doc/hello/README")).unwrap(),
            "docs"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unpack_requires_debian_binary() {
        let dir = std::env::temp_dir().join(format!("deb-bad-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let deb = build_ar(&[("control.tar", b"")]);
        let deb_path = dir.join("bad.deb");
        fs::write(&deb_path, &deb).unwrap();
        let err = unpack(&deb_path, &dir.join("work")).unwrap_err();
        assert!(matches!(err, DebError::Ar(_)));
        let _ = fs::remove_dir_all(&dir);
    }
}
