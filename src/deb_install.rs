//! Installation of Debian `.deb` packages, with a dpkg-compatible status
//! database.
//!
//! This is deliberately self-contained rather than layered on top of `pkg`:
//! `pkg` only accepts *signed* `pkgar` packages from a configured repository,
//! and a `.deb` coming from the outside world cannot be signed with the
//! repository's secret key. Installing `.deb` files therefore keeps its own
//! database, in the same place and format dpkg uses, so that the two can
//! coexist and be inspected with the usual tools.
//!
//! Layout:
//!
//! - `<admindir>/status` — one control stanza per installed package.
//! - `<admindir>/info/<package>.list` — the files the package installed.
//! - `<admindir>/info/<package>.conffiles` — configuration files, preserved
//!   across removal unless the package is purged.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::deb::{self, Control, DebPackage};

/// Errors that can occur while installing a `.deb`.
#[derive(Debug)]
pub enum InstallError {
    /// An I/O failure.
    Io(io::Error),
    /// The package could not be read.
    Deb(deb::DebError),
    /// The package metadata is missing something we need.
    Metadata(String),
    /// A maintainer script failed.
    Script(String),
    /// The requested package is not installed.
    NotInstalled(String),
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InstallError::Io(e) => write!(f, "i/o error: {}", e),
            InstallError::Deb(e) => write!(f, "{}", e),
            InstallError::Metadata(m) => write!(f, "invalid package metadata: {}", m),
            InstallError::Script(m) => write!(f, "maintainer script failed: {}", m),
            InstallError::NotInstalled(m) => write!(f, "package not installed: {}", m),
        }
    }
}

impl std::error::Error for InstallError {}

impl From<io::Error> for InstallError {
    fn from(e: io::Error) -> Self {
        InstallError::Io(e)
    }
}

impl From<deb::DebError> for InstallError {
    fn from(e: deb::DebError) -> Self {
        InstallError::Deb(e)
    }
}

/// Result alias for this module.
pub type Result<T> = std::result::Result<T, InstallError>;

/// Where a `.deb` installation is performed.
///
/// `root` is the filesystem the files are installed into (`/` normally), and
/// `admindir` holds the package database.
#[derive(Debug, Clone)]
pub struct Installer {
    root: PathBuf,
    admindir: PathBuf,
}

impl Installer {
    /// A new installer rooted at `/` with the default admin directory.
    pub fn new() -> Installer {
        Installer::with_paths(Path::new("/"), None)
    }

    /// A new installer with an explicit root and admin directory.
    ///
    /// When `admindir` is `None`, `<root>/var/lib/dpkg` is used.
    pub fn with_paths(root: &Path, admindir: Option<&Path>) -> Installer {
        let admindir = match admindir {
            Some(dir) => dir.to_path_buf(),
            None => root.join("var/lib/dpkg"),
        };
        Installer {
            root: root.to_path_buf(),
            admindir,
        }
    }

    /// The filesystem root files are installed into.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The directory holding the package database.
    pub fn admindir(&self) -> &Path {
        &self.admindir
    }

    fn status_path(&self) -> PathBuf {
        self.admindir.join("status")
    }

    fn info_dir(&self) -> PathBuf {
        self.admindir.join("info")
    }

    /// Read the package database.
    pub fn status(&self) -> Result<BTreeMap<String, Control>> {
        let path = self.status_path();
        let mut packages = BTreeMap::new();
        if !path.is_file() {
            return Ok(packages);
        }
        let text = fs::read_to_string(&path)?;
        for stanza in split_stanzas(&text) {
            let control = Control::parse(&stanza)?;
            if let Some(name) = control.package() {
                packages.insert(name.to_string(), control);
            }
        }
        Ok(packages)
    }

    /// The packages that are currently installed, with their metadata.
    pub fn installed(&self) -> Result<Vec<InstalledPackage>> {
        let mut out = Vec::new();
        for (name, control) in self.status()? {
            let status = control.get("Status").unwrap_or("");
            let installed = status.starts_with("install ok installed");
            let files = self.package_files(&name);
            out.push(InstalledPackage {
                name,
                version: control.version().unwrap_or("").to_string(),
                architecture: control.architecture().unwrap_or("").to_string(),
                status: status.to_string(),
                installed,
                files,
            });
        }
        Ok(out)
    }

    /// The list of files installed by a package.
    pub fn package_files(&self, name: &str) -> Vec<PathBuf> {
        let path = self.info_dir().join(format!("{}.list", name));
        match fs::read_to_string(&path) {
            Ok(text) => text
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(PathBuf::from)
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    /// The configuration files of a package.
    pub fn conffiles(&self, name: &str) -> Vec<PathBuf> {
        let path = self.info_dir().join(format!("{}.conffiles", name));
        match fs::read_to_string(&path) {
            Ok(text) => text
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(PathBuf::from)
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Install a `.deb` file.
    ///
    /// Returns the list of files that were written.
    pub fn install(&self, deb_path: &Path) -> Result<Vec<PathBuf>> {
        let work = self.admindir.join("tmp");
        let _ = fs::remove_dir_all(&work);
        fs::create_dir_all(&work)?;

        let package = deb::unpack(deb_path, &work)?;

        let name = package
            .name()
            .ok_or_else(|| InstallError::Metadata("missing Package field".to_string()))?
            .to_string();
        let version = package.version().unwrap_or("").to_string();

        // A package cannot be installed twice; upgrading is handled by
        // removing the old version first, exactly like dpkg does.
        if self.status()?.contains_key(&name) {
            return Err(InstallError::Metadata(format!(
                "{} is already installed, remove it first",
                name
            )));
        }

        let installed_files = self.copy_tree(&package.data_dir)?;

        // Record the conffiles before running the scripts, so that a failing
        // postinst still leaves a consistent database behind.
        self.write_info(&name, "list", &installed_files)?;
        let conffiles = self.find_conffiles(&package.data_dir);
        if !conffiles.is_empty() {
            self.write_info(&name, "conffiles", &conffiles)?;
        }

        self.run_script(&package, "preinst", "install")?;
        self.write_status(&package, "install ok installed")?;
        if let Err(err) = self.run_script(&package, "postinst", "configure") {
            // The files are in place but the package is not configured, which
            // is exactly the state dpkg leaves behind.
            self.write_status(&package, "install ok half-configured")?;
            return Err(err);
        }

        let _ = fs::remove_dir_all(&work);
        let _ = version;
        Ok(installed_files)
    }

    /// Remove a package, keeping its configuration files.
    pub fn remove(&self, name: &str) -> Result<()> {
        let packages = self.status()?;
        let Some(control) = packages.get(name) else {
            return Err(InstallError::NotInstalled(name.to_string()));
        };

        if let Some(script) = self.read_script(name, "prerm") {
            self.run_script_text(name, control, &script, "remove", "prerm")?;
        }

        // Configuration files are preserved on removal, only a purge
        // removes them.
        let conffiles = self.conffiles(name);
        for file in self.package_files(name) {
            if conffiles.contains(&file) {
                continue;
            }
            let target = self.root.join(&file);
            if target.is_symlink() || target.is_file() {
                let _ = fs::remove_file(&target);
            } else if target.is_dir() {
                let _ = fs::remove_dir(&target);
            }
        }

        if let Some(script) = self.read_script(name, "postrm") {
            self.run_script_text(name, control, &script, "remove", "postrm")?;
        }

        // Configuration files stay behind, so the package is only "config-files".
        let mut updated = control.clone();
        updated.set("Status", "deinstall ok config-files");
        self.write_control(&updated)?;
        Ok(())
    }

    /// Remove a package together with its configuration files.
    pub fn purge(&self, name: &str) -> Result<()> {
        let packages = self.status()?;
        let Some(control) = packages.get(name) else {
            return Err(InstallError::NotInstalled(name.to_string()));
        };

        self.remove(name)?;

        for file in self.conffiles(name) {
            let target = self.root.join(&file);
            let _ = fs::remove_file(&target);
        }

        let mut updated = control.clone();
        updated.set("Status", "purge ok not-installed");
        self.write_control(&updated)?;
        Ok(())
    }

    // -- internals --------------------------------------------------------

    /// Copy the extracted file tree into the root, preserving modes and
    /// symlinks, and return the list of paths that were written.
    fn copy_tree(&self, data_dir: &Path) -> Result<Vec<PathBuf>> {
        let mut written = Vec::new();
        self.copy_tree_into(data_dir, data_dir, &mut written)?;
        written.sort();
        Ok(written)
    }

    fn copy_tree_into(&self, base: &Path, dir: &Path, written: &mut Vec<PathBuf>) -> Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let relative = path.strip_prefix(base).unwrap_or(&path);
            let target = self.root.join(relative);

            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                let link = fs::read_link(&path)?;
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)?;
                }
                let _ = fs::remove_file(&target);
                std::os::unix::fs::symlink(&link, &target)?;
                written.push(relative.to_path_buf());
            } else if file_type.is_dir() {
                fs::create_dir_all(&target)?;
                if let Ok(meta) = fs::metadata(&path) {
                    set_mode(&target, mode_of(&meta));
                }
                self.copy_tree_into(base, &path, written)?;
            } else {
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::copy(&path, &target)?;
                if let Ok(meta) = fs::metadata(&path) {
                    set_mode(&target, mode_of(&meta));
                }
                written.push(relative.to_path_buf());
            }
        }
        Ok(())
    }

    /// Find the files that dpkg would treat as configuration files.
    fn find_conffiles(&self, data_dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for prefix in ["etc"] {
            let dir = data_dir.join(prefix);
            if !dir.is_dir() {
                continue;
            }
            collect_conffiles(&dir, data_dir, &mut out);
        }
        out.sort();
        out
    }

    fn write_info(&self, name: &str, kind: &str, paths: &[PathBuf]) -> Result<()> {
        let dir = self.info_dir();
        fs::create_dir_all(&dir)?;
        let mut text = String::new();
        for path in paths {
            text.push_str(&path.to_string_lossy());
            text.push('\n');
        }
        fs::write(dir.join(format!("{}.{}", name, kind)), text)?;
        Ok(())
    }

    fn read_script(&self, name: &str, script: &str) -> Option<String> {
        let path = self.info_dir().join(format!("{}.{}", name, script));
        fs::read_to_string(path).ok()
    }

    /// Write (or replace) the status entry of a package.
    fn write_status(&self, package: &DebPackage, status: &str) -> Result<()> {
        let mut control = package.control.clone();
        control.set("Status", status);
        if control.get("Installed-Size").is_none() {
            control.set("Installed-Size", "0");
        }
        self.write_control(&control)
    }

    fn write_control(&self, control: &Control) -> Result<()> {
        let name = control
            .package()
            .ok_or_else(|| InstallError::Metadata("missing Package field".to_string()))?;
        let mut packages = self.status()?;
        packages.insert(name.to_string(), control.clone());

        let mut text = String::new();
        for control in packages.values() {
            text.push_str(&control.to_stanza());
            text.push('\n');
        }
        fs::create_dir_all(self.admindir())?;
        fs::write(self.status_path(), text)?;
        Ok(())
    }

    /// The shell that runs the maintainer scripts.
    ///
    /// The shell of the system being installed into is preferred, so that the
    /// script runs against the right C library. When the root does not carry
    /// one, the shell of the running system is used.
    fn resolve_shell(&self) -> PathBuf {
        let candidate = self.root.join("bin/sh");
        if candidate.is_file() {
            return candidate;
        }
        if let Ok(path) = std::env::var("PATH") {
            for dir in path.split(':') {
                if dir.is_empty() {
                    continue;
                }
                let candidate = Path::new(dir).join("sh");
                if candidate.is_file() {
                    return candidate;
                }
            }
        }
        PathBuf::from("/bin/sh")
    }

    /// The `PATH` given to maintainer scripts.
    ///
    /// It points inside the installation root, so that a script finds the
    /// tools of the system it is installing into rather than those of the
    /// machine running the installation.
    fn script_path(&self) -> String {
        if self.root == Path::new("/") {
            return "/usr/sbin:/usr/bin:/sbin:/bin".to_string();
        }
        let root = &self.root;
        [
            root.join("usr/sbin"),
            root.join("usr/bin"),
            root.join("sbin"),
            root.join("bin"),
        ]
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(":")
    }

    fn run_script(&self, package: &DebPackage, script: &str, argument: &str) -> Result<()> {
        let Some(content) = package.scripts.get(script) else {
            return Ok(());
        };
        let name = package.name().unwrap_or("");
        let control = package.control.clone();
        self.run_script_text(name, &control, content, argument, script)
    }

    fn run_script_text(
        &self,
        name: &str,
        control: &Control,
        content: &str,
        argument: &str,
        script_name: &str,
    ) -> Result<()> {
        let script_path = self
            .admindir
            .join("tmp")
            .join(format!("{}.{}", name, argument));
        fs::create_dir_all(script_path.parent().unwrap())?;
        fs::write(&script_path, content)?;

        // Debian maintainer scripts expect this exact environment; the
        // `dpkg-maintscript-helper` utility in particular refuses to run
        // without DPKG_MAINTSCRIPT_NAME.
        let status = Command::new(self.resolve_shell())
            .arg(&script_path)
            .arg(argument)
            .env("DPKG_ROOT", &self.root)
            .env("DPKG_MAINTSCRIPT_PACKAGE", name)
            .env("DPKG_MAINTSCRIPT_NAME", script_name)
            .env(
                "DPKG_MAINTSCRIPT_ARCH",
                control.architecture().unwrap_or(""),
            )
            .env("DPKG_MAINTSCRIPT_INSTDIR", self.root.join("usr/bin"))
            .env("PATH", self.script_path())
            .output()?;

        let _ = fs::remove_file(&script_path);

        if !status.status.success() {
            return Err(InstallError::Script(format!(
                "{} {} failed with {}\n{}",
                name,
                argument,
                status.status,
                String::from_utf8_lossy(&status.stderr).trim()
            )));
        }
        Ok(())
    }
}

impl Default for Installer {
    fn default() -> Self {
        Installer::new()
    }
}

/// A package that is known to the database.
#[derive(Debug, Clone)]
pub struct InstalledPackage {
    /// Package name.
    pub name: String,
    /// Package version.
    pub version: String,
    /// Target architecture.
    pub architecture: String,
    /// The raw dpkg status string.
    pub status: String,
    /// Whether the package is fully installed.
    pub installed: bool,
    /// The files the package installed.
    pub files: Vec<PathBuf>,
}

impl InstalledPackage {
    /// Whether the package has been purged and is no longer known to the
    /// system beyond its empty record.
    pub fn purged(&self) -> bool {
        self.status.starts_with("purge")
    }
}

/// Split a multi-stanza control file into its individual stanzas.
pub fn split_stanzas(text: &str) -> Vec<String> {
    let mut stanzas = Vec::new();
    let mut current = String::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            if !current.trim().is_empty() {
                stanzas.push(std::mem::take(&mut current));
            }
            continue;
        }
        current.push_str(line);
        current.push('\n');
    }
    if !current.trim().is_empty() {
        stanzas.push(current);
    }
    stanzas
}

fn collect_conffiles(dir: &Path, base: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_conffiles(&path, base, out);
        } else if path.is_file()
            && let Ok(relative) = path.strip_prefix(base)
        {
            out.push(relative.to_path_buf());
        }
    }
}

fn mode_of(meta: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o7777
}

fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "deb-install-{}-{}-{:?}",
            tag,
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Build a tar archive in memory.
    fn build_tar(files: &[(&str, &[u8], u32)]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (name, data, mode) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(*mode);
            header.set_cksum();
            builder.append_data(&mut header, name, *data).unwrap();
        }
        builder.into_inner().unwrap()
    }

    fn build_ar(members: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"!<arch>\n");
        for (name, data) in members {
            let mut header = [b' '; 60];
            header[..name.len()].copy_from_slice(name.as_bytes());
            let size = format!("{}", data.len());
            header[48..48 + size.len()].copy_from_slice(size.as_bytes());
            header[58] = 0x60;
            header[59] = b'\n';
            out.extend_from_slice(&header);
            out.extend_from_slice(data);
            if data.len() % 2 == 1 {
                out.push(b'\n');
            }
        }
        out
    }

    /// Build a `.deb` with a control file and a data tree.
    fn make_deb(control: &str, data: &[(&str, &[u8], u32)], scripts: &[(&str, &str)]) -> Vec<u8> {
        let mut control_files: Vec<(&str, &[u8], u32)> =
            vec![("control", control.as_bytes(), 0o644)];
        for (name, body) in scripts {
            control_files.push((name, body.as_bytes(), 0o755));
        }
        let control_tar = build_tar(&control_files);
        let data_tar = build_tar(data);

        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(&control_tar).unwrap();
        let control_gz = enc.finish().unwrap();

        build_ar(&[
            ("debian-binary", b"2.0\n"),
            ("control.tar.gz", &control_gz),
            ("data.tar", &data_tar),
        ])
    }

    #[test]
    fn stanzas_are_split_on_blank_lines() {
        let text = "Package: a\nVersion: 1\n\nPackage: b\nVersion: 2\n";
        let stanzas = split_stanzas(text);
        assert_eq!(stanzas.len(), 2);
        assert!(stanzas[0].contains("Package: a"));
        assert!(stanzas[1].contains("Package: b"));
    }

    #[test]
    fn install_writes_files_and_records_status() {
        let dir = temp_dir("install");
        let root = dir.join("root");
        fs::create_dir_all(&root).unwrap();

        let deb = make_deb(
            "Package: hello\nVersion: 1.0\nArchitecture: amd64\nMaintainer: Test <t@example.com>\n",
            &[
                ("usr/bin/hello", b"#!/bin/sh\necho hi\n", 0o755),
                ("usr/share/doc/hello/README", b"docs", 0o644),
            ],
            &[],
        );
        let deb_path = dir.join("hello.deb");
        fs::write(&deb_path, &deb).unwrap();

        let installer = Installer::with_paths(&root, None);
        let files = installer.install(&deb_path).unwrap();

        assert!(files.contains(&PathBuf::from("usr/bin/hello")));
        assert_eq!(
            fs::read_to_string(root.join("usr/bin/hello")).unwrap(),
            "#!/bin/sh\necho hi\n"
        );
        // The executable bit must survive.
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(root.join("usr/bin/hello"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755);

        // The database knows about it.
        let status = installer.status().unwrap();
        assert!(status.contains_key("hello"));
        assert_eq!(
            status.get("hello").unwrap().get("Status"),
            Some("install ok installed")
        );
        assert_eq!(
            installer.package_files("hello"),
            vec![
                PathBuf::from("usr/bin/hello"),
                PathBuf::from("usr/share/doc/hello/README")
            ]
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_runs_maintainer_scripts() {
        let dir = temp_dir("scripts");
        let root = dir.join("root");
        fs::create_dir_all(&root).unwrap();

        let deb = make_deb(
            "Package: scripted\nVersion: 1.0\nArchitecture: amd64\n",
            &[("usr/share/scripted/marker", b"x", 0o644)],
            &[(
                "postinst",
                "#!/bin/sh\necho \"$1 $DPKG_MAINTSCRIPT_PACKAGE\" > /tmp/dpkg-test-marker\n",
            )],
        );
        let deb_path = dir.join("scripted.deb");
        fs::write(&deb_path, &deb).unwrap();

        let installer = Installer::with_paths(&root, None);
        installer.install(&deb_path).unwrap();

        // The script ran with the expected arguments and environment.
        let marker = fs::read_to_string("/tmp/dpkg-test-marker").unwrap();
        assert_eq!(marker.trim(), "configure scripted");
        let _ = fs::remove_file("/tmp/dpkg-test-marker");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn installing_twice_is_refused() {
        let dir = temp_dir("twice");
        let root = dir.join("root");
        fs::create_dir_all(&root).unwrap();

        let deb = make_deb(
            "Package: twice\nVersion: 1.0\nArchitecture: amd64\n",
            &[("usr/share/twice/f", b"x", 0o644)],
            &[],
        );
        let deb_path = dir.join("twice.deb");
        fs::write(&deb_path, &deb).unwrap();

        let installer = Installer::with_paths(&root, None);
        installer.install(&deb_path).unwrap();
        let err = installer.install(&deb_path).unwrap_err();
        assert!(err.to_string().contains("already installed"), "{}", err);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn remove_deletes_files_but_keeps_configuration() {
        let dir = temp_dir("remove");
        let root = dir.join("root");
        fs::create_dir_all(&root).unwrap();

        let deb = make_deb(
            "Package: removable\nVersion: 1.0\nArchitecture: amd64\n",
            &[
                ("usr/bin/removable", b"binary", 0o755),
                ("etc/removable.conf", b"setting=1\n", 0o644),
            ],
            &[],
        );
        let deb_path = dir.join("removable.deb");
        fs::write(&deb_path, &deb).unwrap();

        let installer = Installer::with_paths(&root, None);
        installer.install(&deb_path).unwrap();
        assert!(root.join("usr/bin/removable").exists());
        assert!(root.join("etc/removable.conf").exists());

        installer.remove("removable").unwrap();

        assert!(!root.join("usr/bin/removable").exists());
        // Configuration files survive a plain removal.
        assert!(root.join("etc/removable.conf").exists());
        assert_eq!(
            installer
                .status()
                .unwrap()
                .get("removable")
                .unwrap()
                .get("Status"),
            Some("deinstall ok config-files")
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn purge_removes_configuration_too() {
        let dir = temp_dir("purge");
        let root = dir.join("root");
        fs::create_dir_all(&root).unwrap();

        let deb = make_deb(
            "Package: purgeable\nVersion: 1.0\nArchitecture: amd64\n",
            &[
                ("usr/bin/purgeable", b"binary", 0o755),
                ("etc/purgeable.conf", b"setting=1\n", 0o644),
            ],
            &[],
        );
        let deb_path = dir.join("purgeable.deb");
        fs::write(&deb_path, &deb).unwrap();

        let installer = Installer::with_paths(&root, None);
        installer.install(&deb_path).unwrap();
        installer.purge("purgeable").unwrap();

        assert!(!root.join("usr/bin/purgeable").exists());
        assert!(!root.join("etc/purgeable.conf").exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn maintainer_script_can_use_the_confmodule() {
        // Debian maintainer scripts source debconf's confmodule to get the
        // db_* shell functions. The stand-in shipped with the apt recipe has
        // to provide them.
        let dir = temp_dir("confmodule");
        let root = dir.join("root");
        fs::create_dir_all(&root).unwrap();

        let confmodule = dir.join("confmodule");
        fs::write(
            &confmodule,
            include_str!("../recipes/core/apt/files/confmodule"),
        )
        .unwrap();

        let answer = dir.join("answer");
        let script = format!(
            "#!/bin/sh\n. \"{}\"\ndb_input high demo/Question Europe/Madrid\ndb_go\ndb_get demo/Question Europe/UTC > \"{}\"\n",
            confmodule.display(),
            answer.display(),
        );

        let deb = make_deb(
            "Package: demo\nVersion: 1.0\nArchitecture: amd64\n",
            &[("usr/share/demo/f", b"x", 0o644)],
            &[("postinst", &script)],
        );
        let deb_path = dir.join("demo.deb");
        fs::write(&deb_path, &deb).unwrap();

        let installer = Installer::with_paths(&root, None);
        installer.install(&deb_path).unwrap();

        // The default answer has to reach the script.
        assert_eq!(fs::read_to_string(&answer).unwrap().trim(), "Europe/Madrid");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn removing_an_absent_package_is_an_error() {
        let dir = temp_dir("absent");
        let root = dir.join("root");
        fs::create_dir_all(&root).unwrap();
        let installer = Installer::with_paths(&root, None);
        let err = installer.remove("nothing").unwrap_err();
        assert!(matches!(err, InstallError::NotInstalled(_)));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn installed_listing_reports_status() {
        let dir = temp_dir("listing");
        let root = dir.join("root");
        fs::create_dir_all(&root).unwrap();

        let deb = make_deb(
            "Package: listed\nVersion: 2.0\nArchitecture: amd64\n",
            &[("usr/share/listed/f", b"x", 0o644)],
            &[],
        );
        let deb_path = dir.join("listed.deb");
        fs::write(&deb_path, &deb).unwrap();

        let installer = Installer::with_paths(&root, None);
        installer.install(&deb_path).unwrap();

        let installed = installer.installed().unwrap();
        assert_eq!(installed.len(), 1);
        assert_eq!(installed[0].name, "listed");
        assert_eq!(installed[0].version, "2.0");
        assert!(installed[0].installed);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn symlinks_are_preserved() {
        let dir = temp_dir("symlink");
        let root = dir.join("root");
        fs::create_dir_all(&root).unwrap();

        // Build a tar with a symlink entry.
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o777);
        header.set_cksum();
        builder
            .append_link(&mut header, "usr/bin/link", "real-target")
            .unwrap();
        let data_tar = builder.into_inner().unwrap();

        let control_tar = build_tar(&[(
            "control",
            b"Package: linked\nVersion: 1.0\nArchitecture: amd64\n",
            0o644,
        )]);
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(&control_tar).unwrap();
        let control_gz = enc.finish().unwrap();

        let deb = build_ar(&[
            ("debian-binary", b"2.0\n"),
            ("control.tar.gz", &control_gz),
            ("data.tar", &data_tar),
        ]);
        let deb_path = dir.join("linked.deb");
        fs::write(&deb_path, &deb).unwrap();

        let installer = Installer::with_paths(&root, None);
        installer.install(&deb_path).unwrap();

        let link = root.join("usr/bin/link");
        assert!(link.is_symlink());
        assert_eq!(fs::read_link(&link).unwrap(), PathBuf::from("real-target"));

        let _ = fs::remove_dir_all(&dir);
    }
}
