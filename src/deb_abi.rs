//! Detection of the C library an ELF binary is linked against.
//!
//! Carcinization uses [relibc](https://gitlab.redox-os.org/redox-os/relibc)
//! rather than glibc. The two are not ABI compatible, so a binary from a
//! Debian or Ubuntu archive cannot run here. This module inspects installed
//! files and reports which ones would be affected, so that the problem is
//! visible instead of mysterious.
//!
//! The ELF reader here is deliberately minimal: it only looks for the
//! `PT_INTERP` segment, which names the dynamic loader the binary asks for.

use std::path::{Path, PathBuf};

/// The C library a binary was linked against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Abi {
    /// Linked against glibc: this will not run on Carcinization.
    Glibc,
    /// Statically linked, or linked against a non-glibc library.
    Other,
}

impl Abi {
    /// A human readable description.
    pub fn describe(self) -> &'static str {
        match self {
            Abi::Glibc => "glibc (incompatible with relibc)",
            Abi::Other => "not glibc",
        }
    }
}

/// What could be learned about a binary.
#[derive(Debug, Clone)]
pub struct BinaryInfo {
    /// The detected C library.
    pub abi: Abi,
    /// The dynamic loader the binary asks for, if any.
    pub interpreter: Option<String>,
}

/// `PT_INTERP` in the ELF program header table.
const PT_INTERP: u32 = 3;

const ELF_MAGIC: &[u8; 4] = b"\x7fELF";

/// Read the `PT_INTERP` string of an ELF image.
///
/// Returns `None` when the data is not an ELF image or has no interpreter
/// (statically linked binaries have none).
pub fn elf_interpreter(data: &[u8]) -> Option<String> {
    if data.len() < 64 || &data[0..4] != ELF_MAGIC {
        return None;
    }

    let is_64 = data[4] == 2;
    let little = data[5] == 1;
    let read_u16 = |offset: usize| -> u64 {
        let bytes = [data[offset], data[offset + 1]];
        let value = if little {
            u16::from_le_bytes(bytes)
        } else {
            u16::from_be_bytes(bytes)
        };
        value as u64
    };
    let read_u32 = |offset: usize| -> u64 {
        let bytes = [
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ];
        let value = if little {
            u32::from_le_bytes(bytes)
        } else {
            u32::from_be_bytes(bytes)
        };
        value as u64
    };
    let read_u64 = |offset: usize| -> u64 {
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&data[offset..offset + 8]);
        if little {
            u64::from_le_bytes(bytes)
        } else {
            u64::from_be_bytes(bytes)
        }
    };

    let (phoff, phentsize, phnum) = if is_64 {
        (read_u64(32), read_u16(54), read_u16(56))
    } else {
        (read_u32(28), read_u16(42), read_u16(44))
    };

    for index in 0..phnum {
        let base = phoff as usize + index as usize * phentsize as usize;
        if base + phentsize as usize > data.len() {
            break;
        }
        let p_type = read_u32(base);
        if p_type != PT_INTERP as u64 {
            continue;
        }
        let (offset, size) = if is_64 {
            (read_u64(base + 8), read_u64(base + 32))
        } else {
            (read_u32(base + 4), read_u32(base + 16))
        };
        let start = offset as usize;
        let end = start + size as usize;
        if end > data.len() {
            break;
        }
        let text: String = data[start..end]
            .iter()
            .take_while(|b| **b != 0)
            .map(|b| *b as char)
            .collect();
        if text.is_empty() {
            break;
        }
        return Some(text);
    }

    None
}

/// Inspect an ELF binary.
///
/// Returns `None` when the file is not an ELF object at all.
pub fn inspect(data: &[u8]) -> Option<BinaryInfo> {
    if data.len() < 4 || &data[0..4] != ELF_MAGIC {
        return None;
    }

    let interpreter = elf_interpreter(data);

    // glibc binaries carry versioned symbol requirements such as
    // "GLIBC_2.34", which is a reliable marker even for binaries that are
    // statically linked against glibc.
    let glibc_marker = data
        .windows(b"GLIBC_2.".len())
        .any(|window| window == b"GLIBC_2.");

    let glibc_interpreter = interpreter
        .as_deref()
        .map(|path| path.contains("ld-linux"))
        .unwrap_or(false);

    let abi = if glibc_marker || glibc_interpreter {
        Abi::Glibc
    } else {
        Abi::Other
    };

    Some(BinaryInfo { abi, interpreter })
}

/// Inspect a file on disk.
pub fn inspect_file(path: &Path) -> Option<BinaryInfo> {
    let data = std::fs::read(path).ok()?;
    inspect(&data)
}

/// Scan a directory tree for ELF binaries and report the glibc-linked ones.
///
/// Returns the paths that would not run on Carcinization.
pub fn find_incompatible(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    scan_dir(root, &mut found);
    found.sort();
    found
}

fn scan_dir(dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            scan_dir(&path, found);
        } else if file_type.is_file()
            && let Some(info) = inspect_file(&path)
            && info.abi == Abi::Glibc
        {
            found.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal 64-bit little endian ELF with one PT_INTERP segment.
    fn build_elf(interp: Option<&str>) -> Vec<u8> {
        let mut data = vec![0u8; 64];
        data[0..4].copy_from_slice(ELF_MAGIC);
        data[4] = 2; // ELFCLASS64
        data[5] = 1; // ELFDATA2LSB
        data[6] = 1; // EV_CURRENT

        let phoff: u64 = 64;
        let phentsize: u64 = 56;
        let phnum: u64 = if interp.is_some() { 1 } else { 0 };
        data[32..40].copy_from_slice(&phoff.to_le_bytes());
        data[54..56].copy_from_slice(&(phentsize as u16).to_le_bytes());
        data[56..58].copy_from_slice(&(phnum as u16).to_le_bytes());

        if let Some(text) = interp {
            let payload = text.as_bytes();
            let offset = 64 + 56;
            let mut header = vec![0u8; 56];
            header[0..4].copy_from_slice(&PT_INTERP.to_le_bytes());
            header[8..16].copy_from_slice(&(offset as u64).to_le_bytes());
            header[32..40].copy_from_slice(&(payload.len() as u64 + 1).to_le_bytes());
            data.extend_from_slice(&header);
            data.extend_from_slice(payload);
            data.push(0);
        }

        data
    }

    #[test]
    fn non_elf_data_is_not_an_object() {
        assert!(inspect(b"this is a text file, not an ELF").is_none());
    }

    #[test]
    fn interpreter_is_read_from_the_program_headers() {
        let elf = build_elf(Some("/lib64/ld-linux-x86-64.so.2"));
        assert_eq!(
            elf_interpreter(&elf).as_deref(),
            Some("/lib64/ld-linux-x86-64.so.2")
        );
    }

    #[test]
    fn glibc_interpreter_is_flagged() {
        let elf = build_elf(Some("/lib64/ld-linux-x86-64.so.2"));
        let info = inspect(&elf).unwrap();
        assert_eq!(info.abi, Abi::Glibc);
    }

    #[test]
    fn static_binaries_are_not_flagged() {
        let elf = build_elf(None);
        let info = inspect(&elf).unwrap();
        assert_eq!(info.abi, Abi::Other);
        assert!(info.interpreter.is_none());
    }

    #[test]
    fn the_glibc_version_marker_is_detected() {
        let mut elf = build_elf(None);
        elf.extend_from_slice(b"\0GLIBC_2.34\0");
        let info = inspect(&elf).unwrap();
        assert_eq!(info.abi, Abi::Glibc);
    }

    #[test]
    fn abi_descriptions_are_distinct() {
        assert_ne!(Abi::Glibc.describe(), Abi::Other.describe());
        assert!(Abi::Glibc.describe().contains("glibc"));
    }

    #[test]
    fn a_real_binary_is_inspected() {
        // Whatever the host is, inspection must succeed and be coherent.
        let path = std::env::current_exe().unwrap();
        let info = inspect_file(&path).expect("the test binary is an ELF");
        if info.abi == Abi::Glibc {
            assert!(info.interpreter.is_some());
        }
    }
}
