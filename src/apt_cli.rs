//! `apt`-compatible frontend for the Carcinization OS package manager.
//!
//! Carcinization ships `pkg` (from the `pkgutils` package) as its package
//! manager, and `pkgar` as its package format. This module provides the
//! familiar Debian command line on top of it, so `apt install <pkg>` behaves
//! the way users expect on any Unix-like system.
//!
//! It is deliberately a thin translator rather than a second implementation:
//! the underlying `pkg` binary remains the single source of truth for what is
//! installed, which keeps the two frontends from ever disagreeing.
//!
//! Three binaries share this implementation and pick their behaviour from
//! `argv[0]`:
//!
//! - `apt`       — interactive frontend.
//! - `apt-get`  — script friendly frontend; never prompts.
//! - `apt-cache` — read-only queries.

use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};

use crate::deb_abi;
use crate::deb_install::Installer;
use crate::deb_repo::{self, DebEntry, PackagesIndex, Source};

/// Which Debian-style frontend this invocation should behave as.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    /// `apt` — the interactive frontend.
    Apt,
    /// `apt-get` — the script friendly frontend; never prompts.
    AptGet,
    /// `apt-cache` — read-only queries against the package cache.
    AptCache,
}

impl Mode {
    /// Derive the mode from the name the binary was invoked as.
    fn from_program(program: &str) -> Mode {
        let name = std::path::Path::new(program)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("apt");
        match name {
            "apt-get" => Mode::AptGet,
            "apt-cache" => Mode::AptCache,
            _ => Mode::Apt,
        }
    }

    /// `apt-get` is documented as never blocking on a prompt.
    fn assumes_yes(self) -> bool {
        matches!(self, Mode::AptGet)
    }
}

/// A request this frontend cannot express with the underlying `pkg` command.
#[derive(Debug, PartialEq, Eq)]
struct Unsupported(String);

impl std::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// What the frontend decided to do.
#[derive(Debug, PartialEq, Eq)]
enum Plan {
    /// Hand these arguments to `pkg`.
    Run(Vec<String>),
    /// Nothing to do; print this message and exit successfully.
    Noop(String),
}

/// Where a `.deb` installation is performed.
#[derive(Debug, Clone)]
pub struct Options {
    /// The filesystem root files are installed into.
    pub root: PathBuf,
    /// The directory holding the package database.
    pub admindir: Option<PathBuf>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            root: PathBuf::from("/"),
            admindir: None,
        }
    }
}

impl Options {
    /// The installer these options describe.
    fn installer(&self) -> Installer {
        Installer::with_paths(&self.root, self.admindir.as_deref())
    }
}

/// The architecture the guest is built for, mapped onto Debian's name.
fn debian_architecture() -> Option<&'static str> {
    #[cfg(target_arch = "x86_64")]
    let arch = "x86_64";
    #[cfg(target_arch = "aarch64")]
    let arch = "aarch64";
    #[cfg(target_arch = "riscv64")]
    let arch = "riscv64gc";
    #[cfg(not(any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "riscv64"
    )))]
    let arch = "";

    deb_repo::debian_arch(arch)
}

const HELP: &str = "\
Usage: apt [OPTION...] COMMAND

Carcinization OS package manager (Debian-compatible frontend for `pkg`).

Commands:
  install PACKAGE...   install one or more packages
  remove PACKAGE...    remove one or more packages
  update               refresh the package cache and upgrade installed packages
  upgrade              upgrade installed packages to their newest version
  search TERM          search for packages matching TERM
  show PACKAGE         show information about a package
  list                 list installed packages
  policy PACKAGE       show the installed and available version of a package
  clean                clear the local package cache (no-op: `pkg` is stateless)

Options:
  -y, --yes            assume yes for all prompts
      --curl           use the cURL download backend
      --nocheck        do not preserve locally modified files
  -q, --quiet          accepted for compatibility; `pkg` has no quiet mode
  -h, --help           show this help
  -V, --version        show version information
";

/// Translate an apt-style command line into a `pkg` invocation.
///
/// Returns `Err` for anything that cannot be honoured faithfully — silently
/// doing something else (notably: installing a package when the user asked
/// for a dry run) would be far worse than refusing.
fn translate(args: &[String], mode: Mode) -> Result<Plan, Unsupported> {
    let mut pkg_flags: Vec<String> = Vec::new();
    let mut rest: Vec<String> = Vec::new();

    for arg in args.iter().cloned() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(Plan::Noop(HELP.to_string())),
            "-V" | "--version" => {
                return Ok(Plan::Noop(format!(
                    "apt {} (Carcinization OS frontend for pkg)",
                    env!("CARGO_PKG_VERSION")
                )));
            }
            "-y" | "--yes" => {
                if !pkg_flags.iter().any(|f| f == "--yes") {
                    pkg_flags.push("--yes".to_string());
                }
            }
            "--curl" => pkg_flags.push("--curl".to_string()),
            "--nocheck" => pkg_flags.push("--nocheck".to_string()),
            // `pkg` has no quiet mode and no dry-run mode. The former is
            // harmless to drop, the latter must not be.
            "-q" | "--quiet" => {}
            "-s" | "--simulate" | "--dry-run" => {
                return Err(Unsupported(format!(
                    "{}: --simulate/--dry-run is not supported, the pkg backend cannot \
                     resolve dependencies without performing the transaction",
                    mode_name(mode)
                )));
            }
            _ => rest.push(arg),
        }
    }

    let Some(command) = rest.first().cloned() else {
        return Err(Unsupported(format!(
            "{}: no command given, try '{} --help'",
            mode_name(mode),
            mode_name(mode)
        )));
    };
    let operands: Vec<String> = rest[1..].to_vec();

    if mode.assumes_yes() && !pkg_flags.iter().any(|f| f == "--yes") {
        pkg_flags.push("--yes".to_string());
    }

    // Read-only queries never need the `--yes` flag.
    let read_only = is_read_only_command(&command);
    if read_only {
        pkg_flags.retain(|f| f != "--yes");
    }

    // `apt-cache` is a read-only frontend, it must never mutate the system.
    if mode == Mode::AptCache && !read_only {
        return Err(Unsupported(format!(
            "{}: '{}' is not a read-only command, use 'apt' or 'apt-get' instead",
            mode_name(mode),
            command
        )));
    }

    // Commands that do not map onto a `pkg` subcommand are handled before the
    // translation, so that they never reach the lookup below.
    match command.as_str() {
        "clean" | "autoclean" => {
            return Ok(Plan::Noop(
                "The pkg backend is stateless, there is no local cache to clean.".to_string(),
            ));
        }
        "stats" | "dumpavail" => {
            return Ok(Plan::Noop(
                "The pkg backend does not track cache statistics.".to_string(),
            ));
        }
        "moo" => {
            return Ok(Plan::Noop(
                "Super Cow.\n         ....\n         ....\n         ....\n         ....\n\
                 ..........\n         ...........\n         .........."
                    .to_string(),
            ));
        }
        "source" => {
            return Err(Unsupported(format!(
                "{}: 'source' is not supported, the pkg backend installs binary packages only",
                mode_name(mode)
            )));
        }
        _ => {}
    }

    let mut pkg_args = pkg_flags;
    pkg_args.push(command_pkg_name(&command, mode)?);

    match command.as_str() {
        "install" => {
            if operands.is_empty() {
                return Err(Unsupported(format!(
                    "{}: no packages specified, try '{} install <package>'",
                    mode_name(mode),
                    mode_name(mode)
                )));
            }
            pkg_args.extend(operands);
        }
        "remove" | "rm" | "purge" | "autoremove" => {
            if operands.is_empty() {
                return Err(Unsupported(format!(
                    "{}: no packages specified, try '{} remove <package>'",
                    mode_name(mode),
                    mode_name(mode)
                )));
            }
            pkg_args.extend(operands);
        }
        "update" | "upgrade" | "full-upgrade" | "dist-upgrade" => {
            // `pkg update` with no operands upgrades everything installed.
            pkg_args.extend(operands);
        }
        "search" => {
            let Some(term) = operands.first() else {
                return Err(Unsupported(format!(
                    "{}: search requires a term",
                    mode_name(mode)
                )));
            };
            pkg_args.push(term.clone());
        }
        "show" | "info" | "policy" => {
            let Some(package) = operands.first() else {
                return Err(Unsupported(format!(
                    "{}: {} requires a package name",
                    mode_name(mode),
                    command
                )));
            };
            pkg_args.push(package.clone());
        }
        "list" => {}
        other => {
            return Err(Unsupported(format!(
                "{}: invalid command '{}'",
                mode_name(mode),
                other
            )));
        }
    }

    Ok(Plan::Run(pkg_args))
}

/// Whether an apt subcommand only reads state, without mutating the system.
fn is_read_only_command(command: &str) -> bool {
    matches!(
        command,
        "search" | "show" | "info" | "list" | "policy" | "stats" | "dumpavail"
    )
}

/// Map an apt subcommand onto the `pkg` subcommand that implements it.
fn command_pkg_name(command: &str, mode: Mode) -> Result<String, Unsupported> {
    let name = match command {
        "install" => "install",
        "remove" | "rm" | "purge" | "autoremove" => "remove",
        "update" | "upgrade" | "full-upgrade" | "dist-upgrade" => "update",
        "search" => "search",
        "show" | "info" | "policy" => "info",
        "list" => "list",
        other => {
            return Err(Unsupported(format!(
                "{}: invalid command '{}'",
                mode_name(mode),
                other
            )));
        }
    };
    Ok(name.to_string())
}

/// The name to use when reporting errors for this mode.
fn mode_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Apt => "apt",
        Mode::AptGet => "apt-get",
        Mode::AptCache => "apt-cache",
    }
}

/// Locate the `pkg` binary.
fn find_pkg() -> String {
    if let Ok(path) = std::env::var("PATH") {
        for dir in path.split(':') {
            if dir.is_empty() {
                continue;
            }
            let candidate = std::path::Path::new(dir).join("pkg");
            if candidate.is_file() {
                return candidate.to_string_lossy().into_owned();
            }
        }
    }
    "/usr/bin/pkg".to_string()
}

// ---------------------------------------------------------------------------
// .deb handling
// ---------------------------------------------------------------------------

/// Split the installation options out of the command line.
fn split_install_options(args: &[String]) -> (Options, Vec<String>) {
    let mut options = Options::default();
    let mut rest = Vec::new();
    let mut iter = args.iter().cloned();

    while let Some(arg) = iter.next() {
        let mut take_value = |target: &mut Option<String>| {
            if let Some(value) = iter.next() {
                *target = Some(value);
            }
        };
        let mut root_value: Option<String> = None;
        let mut admindir_value: Option<String> = None;

        if let Some(value) = arg.strip_prefix("--root=") {
            root_value = Some(value.to_string());
        } else if arg == "--root" {
            take_value(&mut root_value);
        } else if let Some(value) = arg.strip_prefix("--admindir=") {
            admindir_value = Some(value.to_string());
        } else if arg == "--admindir" {
            take_value(&mut admindir_value);
        } else {
            rest.push(arg);
            continue;
        }

        if let Some(value) = root_value {
            options.root = PathBuf::from(value);
        }
        if let Some(value) = admindir_value {
            options.admindir = Some(PathBuf::from(value));
        }
    }

    (options, rest)
}

/// The Debian sources configured in `sources.list`.
fn configured_sources(options: &Options) -> Vec<Source> {
    let path = deb_repo::sources_list_path(&options.root);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let arch = debian_architecture().unwrap_or("amd64");
    deb_repo::parse_sources_list(&text)
        .into_iter()
        .map(|mut source| {
            source.arch = arch.to_string();
            source
        })
        .collect()
}

/// Where the downloaded `Packages` index is cached.
fn index_cache_path(options: &Options, source: &Source) -> PathBuf {
    options
        .root
        .join("var/cache/apt")
        .join(format!("Packages-{}-{}.gz", source.suite, source.arch))
}

/// Load the `Packages` index, refreshing the cache when it is missing.
fn packages_index(options: &Options) -> Option<(PackagesIndex, Vec<Source>)> {
    let sources = configured_sources(options);
    if sources.is_empty() {
        return None;
    }

    let mut index = PackagesIndex::default();
    for source in &sources {
        let cache = index_cache_path(options, source);
        let text = if cache.is_file() {
            deb_repo::read_maybe_gzip(&cache).ok()?
        } else {
            // The index is missing, fetch it and keep it compressed on disk.
            deb_repo::fetch_index_gz(source, &cache).ok()?;
            deb_repo::read_maybe_gzip(&cache).ok()?
        };
        let parsed = PackagesIndex::parse(&text).ok()?;
        index.merge(parsed);
    }

    Some((index, sources))
}

/// Install a `.deb` file from disk.
fn install_debs(paths: &[&String], options: &Options) -> Result<ExitCode, String> {
    let installer = options.installer();
    for path in paths {
        println!("Reading package lists... Done");
        println!("Installing {} ...", path);
        match installer.install(std::path::Path::new(path)) {
            Ok(files) => {
                println!("Installed {} ({} files).", path, files.len());
                report_incompatible(&installer);
            }
            Err(err) => return Err(format!("apt: {}", err)),
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Download a package from the mirror and install it.
fn install_from_mirror(
    entry: &DebEntry,
    sources: &[Source],
    options: &Options,
) -> Result<ExitCode, String> {
    let work = options.root.join("var/cache/apt/archives");
    std::fs::create_dir_all(&work).map_err(|e| format!("apt: {}", e))?;
    let deb_path = work.join(format!(
        "{}_{}_{}.deb",
        entry.package,
        entry.version.replace(':', "-"),
        entry.architecture
    ));

    let mut downloaded = false;
    for source in sources {
        let url = format!("{}/{}", source.url.trim_end_matches('/'), entry.filename);
        if deb_repo::download(&url, &deb_path).is_ok() {
            downloaded = true;
            break;
        }
    }
    if !downloaded {
        return Err(format!(
            "apt: failed to download {} ({}) from the configured mirrors",
            entry.package, entry.filename
        ));
    }

    if let Err(err) = deb_repo::verify_sha256(&deb_path, &entry.sha256) {
        let _ = std::fs::remove_file(&deb_path);
        return Err(format!("apt: {}", err));
    }

    println!("Installing {} ({}) ...", entry.package, entry.version);
    let installer = options.installer();
    match installer.install(&deb_path) {
        Ok(files) => {
            println!("Installed {} ({} files).", entry.package, files.len());
            report_incompatible(&installer);
            Ok(ExitCode::SUCCESS)
        }
        Err(err) => Err(format!("apt: {}", err)),
    }
}

/// Report the installed binaries that will not run because they are glibc
/// linked, while the guest uses relibc.
fn report_incompatible(installer: &Installer) {
    for package in installer.installed().unwrap_or_default() {
        if !package.installed {
            continue;
        }
        let mut incompatible = Vec::new();
        for file in &package.files {
            let path = installer.root().join(file);
            if let Some(info) = deb_abi::inspect_file(&path)
                && info.abi == deb_abi::Abi::Glibc
            {
                incompatible.push(file.clone());
            }
        }
        if !incompatible.is_empty() {
            println!(
                "Note: {} ships {} glibc-linked binary(s) that will not run on this system:",
                package.name,
                incompatible.len()
            );
            for file in incompatible.iter().take(10) {
                println!("  {}", file.display());
            }
            if incompatible.len() > 10 {
                println!("  ... and {} more", incompatible.len() - 10);
            }
            println!(
                "Carcinization uses relibc, not glibc. These binaries have to be rebuilt \
                 from their Debian source package to work here."
            );
        }
    }
}

/// Remove or purge a package installed from a `.deb`.
fn remove_deb(name: &str, purge: bool, options: &Options) -> Result<ExitCode, String> {
    let installer = options.installer();
    let result = if purge {
        installer.purge(name)
    } else {
        installer.remove(name)
    };
    match result {
        Ok(()) => {
            println!("{} {} ", if purge { "Purging" } else { "Removing" }, name);
            Ok(ExitCode::SUCCESS)
        }
        Err(err) => Err(format!("apt: {}", err)),
    }
}

/// Print the packages installed from `.deb` files.
fn print_deb_list(packages: &[crate::deb_install::InstalledPackage]) -> ExitCode {
    println!("Listing...");
    for package in packages {
        if !package.installed {
            continue;
        }
        println!(
            "{}/{} {} {}",
            package.name, package.architecture, package.version, package.status
        );
    }
    ExitCode::SUCCESS
}

/// Print the metadata of an installed `.deb`.
fn print_deb_info(package: &crate::deb_install::InstalledPackage) -> ExitCode {
    println!("Package: {}", package.name);
    println!("Version: {}", package.version);
    println!("Architecture: {}", package.architecture);
    println!("Status: {}", package.status);
    println!("Installed files: {}", package.files.len());
    ExitCode::SUCCESS
}

/// Print what the mirror knows about a package.
fn print_index_info(entry: &DebEntry) -> ExitCode {
    println!("Package: {}", entry.package);
    println!("Version: {}", entry.version);
    println!("Architecture: {}", entry.architecture);
    println!("Size: {}", entry.size);
    if !entry.depends.is_empty() {
        println!("Depends: {}", entry.depends.join(", "));
    }
    println!("Filename: {}", entry.filename);
    ExitCode::SUCCESS
}

/// Refresh the cached `Packages` indexes.
fn update_indexes(options: &Options) -> Result<ExitCode, String> {
    let sources = configured_sources(options);
    if sources.is_empty() {
        return Err(format!(
            "apt: no Debian sources configured in {}",
            deb_repo::sources_list_path(&options.root).display()
        ));
    }
    for source in &sources {
        println!("Get:1 {}", source.index_url());
        let cache = index_cache_path(options, source);
        // The index is stored as it was downloaded, so that refreshing it does
        // not mean decompressing and recompressing tens of megabytes.
        deb_repo::fetch_index_gz(source, &cache).map_err(|e| format!("apt: {e}"))?;
        println!("Reading package lists... Done");
    }
    Ok(ExitCode::SUCCESS)
}

/// Handle the operations that concern `.deb` packages.
///
/// Returns `None` when the command is not about `.deb` packages and should be
/// handled by `pkg` instead.
fn deb_dispatch(
    args: &[String],
    mode: Mode,
    options: &Options,
) -> Option<Result<ExitCode, String>> {
    let mut command: Option<String> = None;
    let mut operands: Vec<String> = Vec::new();
    for arg in args {
        if command.is_none() {
            if arg.starts_with('-') {
                continue;
            }
            command = Some(arg.clone());
        } else {
            operands.push(arg.clone());
        }
    }
    let command = command?;
    let installer = options.installer();

    match command.as_str() {
        "install" => {
            let debs: Vec<&String> = operands
                .iter()
                .filter(|operand| operand.ends_with(".deb"))
                .collect();
            if !debs.is_empty() {
                return Some(install_debs(&debs, options));
            }
            None
        }
        "remove" | "rm" | "purge" | "autoremove" => {
            let Ok(installed) = installer.installed() else {
                return None;
            };
            for name in &operands {
                if installed.iter().any(|p| p.name == *name && !p.purged()) {
                    return Some(remove_deb(name, command == "purge", options));
                }
            }
            None
        }
        "list" => {
            let Ok(installed) = installer.installed() else {
                return None;
            };
            let debs: Vec<crate::deb_install::InstalledPackage> =
                installed.into_iter().filter(|p| p.installed).collect();
            if debs.is_empty() {
                return None;
            }
            Some(Ok(print_deb_list(&debs)))
        }
        "show" | "info" | "policy" => {
            let name = operands.first()?;
            if let Ok(installed) = installer.installed()
                && let Some(package) = installed.iter().find(|p| p.name == *name)
            {
                return Some(Ok(print_deb_info(package)));
            }
            let (index, _) = packages_index(options)?;
            index.get(name).map(|entry| Ok(print_index_info(entry)))
        }
        "update" => {
            if configured_sources(options).is_empty() {
                return None;
            }
            Some(update_indexes(options))
        }
        _ => {
            let _ = mode;
            None
        }
    }
}

/// Entry point shared by the `apt`, `apt-get` and `apt-cache` binaries.
pub fn run() -> ExitCode {
    let mut argv = std::env::args();
    let program = argv.next().unwrap_or_else(|| "apt".to_string());
    let mode = Mode::from_program(&program);
    let args: Vec<String> = argv.collect();

    // Options that describe where packages are installed are consumed here;
    // `pkg` does not know about them.
    let (options, rest) = split_install_options(&args);

    // `.deb` packages are handled here, everything else is delegated to `pkg`.
    match deb_dispatch(&rest, mode, &options) {
        Some(Ok(code)) => return code,
        Some(Err(message)) => {
            eprintln!("{}", message);
            return ExitCode::from(1);
        }
        None => {}
    }

    let plan = match translate(&rest, mode) {
        Ok(plan) => plan,
        Err(err) => {
            eprintln!("{}", err);
            return ExitCode::from(1);
        }
    };

    match plan {
        Plan::Noop(message) => {
            println!("{}", message);
            ExitCode::SUCCESS
        }
        Plan::Run(pkg_args) => {
            // The native `pkgar` packages are built against relibc and work.
            // Only the packages the native repository does not provide are
            // taken from a Debian archive, whose binaries need glibc and
            // therefore do not run here without being rebuilt.
            //
            // Which repository has the package is decided by asking the
            // native repository, because `pkg` reports a package it does not
            // have with an error message but still exits successfully.
            if pkg_args.first().map(|a| a.as_str()) == Some("install")
                && let Some(code) = install_missing_from_mirror(&rest, &options, mode)
            {
                return code;
            }

            let code = run_pkg(&pkg_args, mode);
            if code == ExitCode::SUCCESS {
                return code;
            }

            // `pkg` failed for another reason than a missing package, so the
            // Debian archive gets the last word.
            if pkg_args.first().map(|a| a.as_str()) == Some("install")
                && let Some(code) = install_from_mirror_fallback(&rest, &options)
            {
                return code;
            }
            code
        }
    }
}

/// Run `pkg` with the given arguments and report its exit status.
fn run_pkg(args: &[String], mode: Mode) -> ExitCode {
    match Command::new(find_pkg()).args(args).status() {
        Ok(status) => match status.code() {
            Some(code) => ExitCode::from((code & 0xff) as u8),
            None => {
                eprintln!("{}: pkg was terminated by a signal", mode_name(mode));
                ExitCode::from(1)
            }
        },
        Err(err) => {
            eprintln!("{}: failed to run 'pkg': {}", mode_name(mode), err);
            ExitCode::from(1)
        }
    }
}

/// Install the requested packages that the native repository does not provide.
///
/// The packages the native repository does have are left to `pkg`, which
/// installs them from the relibc linked archives.
///
/// Returns `None` when every requested package is available natively, so that
/// the caller can hand the whole request to `pkg`.
fn install_missing_from_mirror(args: &[String], options: &Options, mode: Mode) -> Option<ExitCode> {
    let names = requested_packages(args);
    if names.is_empty() {
        return None;
    }

    let mut native = Vec::new();
    let mut missing = Vec::new();
    for name in names {
        match native_repo_has(name) {
            Some(false) => missing.push(name),
            _ => native.push(name),
        }
    }
    if missing.is_empty() {
        return None;
    }

    let mut failed = false;
    for name in &missing {
        match install_one_from_mirror(name, options) {
            Some(code) => {
                if code != ExitCode::SUCCESS {
                    failed = true;
                }
            }
            None => {
                eprintln!(
                    "apt: '{}' is not available in the native repository nor in the Debian archives",
                    name
                );
                failed = true;
            }
        }
    }

    if !native.is_empty() {
        let mut install_args = vec!["install".to_string()];
        install_args.extend(native.iter().map(|name| name.to_string()));
        if run_pkg(&install_args, mode) != ExitCode::SUCCESS {
            failed = true;
        }
    }

    Some(if failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

/// Try to satisfy an install request from the configured Debian mirrors.
///
/// Returns `None` when the mirror does not know the package either.
fn install_from_mirror_fallback(args: &[String], options: &Options) -> Option<ExitCode> {
    let name = requested_packages(args).first().copied()?;
    install_one_from_mirror(name, options)
}

/// Install a single package from the Debian mirrors.
///
/// Returns `None` when no configured mirror knows the package.
fn install_one_from_mirror(name: &str, options: &Options) -> Option<ExitCode> {
    let (index, sources) = packages_index(options)?;
    let entry = index.get(name)?;

    eprintln!(
        "Note: '{}' is not available as a Carcinization package, \
         installing the Debian archive instead.",
        name
    );

    match install_from_mirror(entry, &sources, options) {
        Ok(code) => Some(code),
        Err(message) => {
            eprintln!("{}", message);
            Some(ExitCode::from(1))
        }
    }
}

/// The package names an install request asks for.
///
/// The first non-flag argument is the command itself, the rest are package
/// names.
fn requested_packages(args: &[String]) -> Vec<&str> {
    args.iter()
        .filter(|arg| !arg.starts_with('-'))
        .skip(1)
        .map(|arg| arg.as_str())
        .collect()
}

/// Extract the package names from the output of `pkg search`.
///
/// The output is a numbered list such as `12: relibc`. Only the index may be
/// coloured, so the name itself is plain text.
fn package_names(output: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(output)
        .lines()
        .filter_map(|line| line.split_once(": "))
        .map(|(_, name)| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .collect()
}

/// Whether the native package repository provides a package with this exact
/// name.
///
/// `pkg` reports a package that its repository does not have with an error
/// message, but still exits with a success status, so its exit code cannot be
/// used to find out whether a request was satisfied. The repository is asked
/// directly instead.
///
/// Returns `None` when the repository cannot be queried at all, so that the
/// caller can let `pkg` try the request itself.
fn native_repo_has(name: &str) -> Option<bool> {
    let output = Command::new(find_pkg())
        .arg("search")
        .arg(name)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(
        package_names(&output.stdout)
            .iter()
            .any(|found| found == name),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn run(list: &[&str]) -> Result<Plan, Unsupported> {
        translate(&args(list), Mode::Apt)
    }

    fn pkg_of(list: &[&str]) -> Vec<String> {
        match run(list) {
            Ok(Plan::Run(v)) => v,
            other => panic!("expected a run plan, got {:?}", other),
        }
    }

    #[test]
    fn install_translates_to_pkg_install() {
        assert_eq!(pkg_of(&["install", "curl"]), vec!["install", "curl"]);
        assert_eq!(
            pkg_of(&["install", "curl", "git"]),
            vec!["install", "curl", "git"]
        );
    }

    #[test]
    fn remove_variants_translate_to_pkg_remove() {
        for cmd in ["remove", "rm", "purge", "autoremove"] {
            assert_eq!(pkg_of(&[cmd, "vim"]), vec!["remove", "vim"]);
        }
    }

    #[test]
    fn update_and_upgrade_translate_to_pkg_update() {
        assert_eq!(pkg_of(&["update"]), vec!["update"]);
        assert_eq!(pkg_of(&["upgrade"]), vec!["update"]);
        assert_eq!(pkg_of(&["full-upgrade"]), vec!["update"]);
        assert_eq!(pkg_of(&["dist-upgrade"]), vec!["update"]);
    }

    #[test]
    fn queries_translate() {
        assert_eq!(pkg_of(&["search", "curl"]), vec!["search", "curl"]);
        assert_eq!(pkg_of(&["show", "curl"]), vec!["info", "curl"]);
        assert_eq!(pkg_of(&["policy", "curl"]), vec!["info", "curl"]);
        assert_eq!(pkg_of(&["list"]), vec!["list"]);
    }

    #[test]
    fn global_flags_are_forwarded_before_the_subcommand() {
        assert_eq!(
            pkg_of(&["--curl", "install", "curl"]),
            vec!["--curl", "install", "curl"]
        );
        assert_eq!(
            pkg_of(&["-y", "install", "curl"]),
            vec!["--yes", "install", "curl"]
        );
        assert_eq!(
            pkg_of(&["install", "curl", "--nocheck"]),
            vec!["--nocheck", "install", "curl"]
        );
    }

    #[test]
    fn yes_is_not_duplicated() {
        assert_eq!(
            pkg_of(&["--yes", "-y", "install", "curl"]),
            vec!["--yes", "install", "curl"]
        );
    }

    #[test]
    fn quiet_is_accepted_and_dropped() {
        assert_eq!(pkg_of(&["-q", "list"]), vec!["list"]);
        assert_eq!(pkg_of(&["--quiet", "list"]), vec!["list"]);
    }

    #[test]
    fn read_only_commands_never_carry_yes() {
        assert_eq!(pkg_of(&["-y", "search", "curl"]), vec!["search", "curl"]);
        assert_eq!(pkg_of(&["--yes", "list"]), vec!["list"]);
    }

    #[test]
    fn dry_run_is_refused_rather_than_ignored() {
        for flag in ["-s", "--simulate", "--dry-run"] {
            let err = translate(&args(&[flag, "install", "curl"]), Mode::Apt)
                .expect_err("dry run must not be silently ignored");
            assert!(err.to_string().contains("not supported"), "{}", err);
        }
    }

    #[test]
    fn install_without_packages_is_an_error() {
        assert!(run(&["install"]).is_err());
        assert!(run(&["remove"]).is_err());
    }

    #[test]
    fn unknown_command_is_an_error() {
        assert!(run(&["frobnicate"]).is_err());
    }

    #[test]
    fn clean_is_a_noop() {
        assert!(matches!(run(&["clean"]), Ok(Plan::Noop(_))));
    }

    #[test]
    fn source_is_refused() {
        let err = translate(&args(&["source", "curl"]), Mode::Apt).expect_err("source unsupported");
        assert!(err.to_string().contains("not supported"), "{}", err);
    }

    #[test]
    fn help_and_version_are_noops() {
        assert!(matches!(run(&["--help"]), Ok(Plan::Noop(_))));
        assert!(matches!(run(&["--version"]), Ok(Plan::Noop(_))));
    }

    #[test]
    fn apt_get_always_assumes_yes() {
        let plan = translate(&args(&["install", "curl"]), Mode::AptGet).unwrap();
        let Plan::Run(v) = plan else {
            panic!("expected a run plan");
        };
        assert_eq!(v, vec!["--yes", "install", "curl"]);
    }

    #[test]
    fn apt_get_update_does_not_need_yes() {
        // `update` is a transaction, so it keeps --yes.
        let plan = translate(&args(&["update"]), Mode::AptGet).unwrap();
        let Plan::Run(v) = plan else {
            panic!("expected a run plan");
        };
        assert_eq!(v, vec!["--yes", "update"]);
    }

    #[test]
    fn apt_cache_is_read_only() {
        let plan = translate(&args(&["search", "curl"]), Mode::AptCache).unwrap();
        let Plan::Run(v) = plan else {
            panic!("expected a run plan");
        };
        assert_eq!(v, vec!["search", "curl"]);
    }

    #[test]
    fn apt_cache_refuses_to_mutate() {
        for cmd in ["install", "remove", "purge", "update", "upgrade"] {
            let err = translate(&args(&[cmd, "curl"]), Mode::AptCache)
                .expect_err("apt-cache must not mutate the system");
            assert!(err.to_string().contains("read-only"), "{}", err);
        }
    }

    #[test]
    fn apt_get_may_mutate() {
        assert!(translate(&args(&["install", "curl"]), Mode::AptGet).is_ok());
        assert!(translate(&args(&["remove", "curl"]), Mode::AptGet).is_ok());
    }

    #[test]
    fn mode_is_derived_from_program_name() {
        assert_eq!(Mode::from_program("/usr/bin/apt-get"), Mode::AptGet);
        assert_eq!(Mode::from_program("/usr/bin/apt-cache"), Mode::AptCache);
        assert_eq!(Mode::from_program("apt"), Mode::Apt);
        assert_eq!(Mode::from_program("/usr/bin/apt"), Mode::Apt);
    }

    #[test]
    fn requested_packages_skip_the_command() {
        assert_eq!(
            requested_packages(&args(&["install", "hello"])),
            vec!["hello"]
        );
        assert_eq!(
            requested_packages(&args(&["install", "hello", "curl"])),
            vec!["hello", "curl"]
        );
        assert!(requested_packages(&args(&["install"])).is_empty());
    }

    #[test]
    fn package_names_are_read_from_pkg_search_output() {
        let output = b"1: nushell\n2: hello-redox\n3: relibc\n";
        assert_eq!(
            package_names(output),
            vec!["nushell", "hello-redox", "relibc"]
        );
    }

    /// `pkg search` matches on substrings, so `hello` also lists
    /// `hello-redox`. Only an exact match counts as the package being
    /// available.
    #[test]
    fn package_names_are_matched_exactly() {
        let names = package_names(b"1: nushell\n2: hello-redox\n");
        assert!(!names.iter().any(|found| found == "hello"));
        assert!(names.iter().any(|found| found == "hello-redox"));
    }
}
