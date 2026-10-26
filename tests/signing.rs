//! Round trip of the pkgar signing machinery.
//!
//! Packages are signed with the secret key kept in `build/id_ed25519.toml`
//! and are verified by clients with the matching public key. This test makes
//! sure that a package really is signed, that it verifies with the key that
//! was published, and that it does *not* verify with a different one.

use std::fs;
use std::path::{Path, PathBuf};

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pkgar-signing-{}-{}", tag, std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_source(dir: &Path) {
    fs::create_dir_all(dir.join("usr/bin")).unwrap();
    fs::write(dir.join("usr/bin/hello"), b"#!/bin/sh\necho hi\n").unwrap();
}

#[test]
fn a_signed_package_verifies_only_with_its_own_key() {
    let dir = temp_dir("roundtrip");
    let source = dir.join("source");
    write_source(&source);

    // The key pair is generated the same way the build system does it.
    let secret_path = dir.join("id_ed25519.toml");
    let public_path = dir.join("id_ed25519.pub.toml");
    let (public_key, secret_key) = pkgar_keys::SecretKeyFile::new();
    public_key.save(&public_path).unwrap();
    secret_key.save(&secret_path).unwrap();

    let archive = dir.join("hello.pkgar");
    pkgar::create_with_flags(
        &secret_path,
        &archive,
        &source,
        pkgar_core::HeaderFlags::latest(
            pkgar_core::Architecture::Independent,
            pkgar_core::Packaging::Uncompressed,
        ),
    )
    .unwrap();

    assert!(archive.is_file());

    // The package opens with the published key, and its contents verify.
    let pkey = pkgar_keys::PublicKeyFile::open(&public_path).unwrap().pkey;
    let mut package = pkgar::PackageFile::new(&archive, &pkey).unwrap();
    package.verify(&source).unwrap();

    // A different key must not open it.
    let (other_public, other_secret) = pkgar_keys::SecretKeyFile::new();
    let other_public_path = dir.join("other.pub.toml");
    other_public.save(&other_public_path).unwrap();
    other_secret.save(dir.join("other.toml")).unwrap();
    let other_pkey = pkgar_keys::PublicKeyFile::open(&other_public_path)
        .unwrap()
        .pkey;
    assert!(
        pkgar::PackageFile::new(&archive, &other_pkey).is_err(),
        "a package must not open with a foreign key"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_tampered_file_fails_verification() {
    let dir = temp_dir("tampered");
    let source = dir.join("source");
    write_source(&source);

    let secret_path = dir.join("id_ed25519.toml");
    let public_path = dir.join("id_ed25519.pub.toml");
    let (public_key, secret_key) = pkgar_keys::SecretKeyFile::new();
    public_key.save(&public_path).unwrap();
    secret_key.save(&secret_path).unwrap();

    let archive = dir.join("hello.pkgar");
    pkgar::create_with_flags(
        &secret_path,
        &archive,
        &source,
        pkgar_core::HeaderFlags::latest(
            pkgar_core::Architecture::Independent,
            pkgar_core::Packaging::Uncompressed,
        ),
    )
    .unwrap();

    // Change a file after signing it.
    fs::write(source.join("usr/bin/hello"), b"#!/bin/sh\necho pwned\n").unwrap();

    let pkey = pkgar_keys::PublicKeyFile::open(&public_path).unwrap().pkey;
    let mut package = pkgar::PackageFile::new(&archive, &pkey).unwrap();
    assert!(
        package.verify(&source).is_err(),
        "a modified file must not verify"
    );

    let _ = fs::remove_dir_all(&dir);
}
