pub mod apt_cli;
pub mod config;
pub mod cook;
pub mod deb;
pub mod deb_abi;
pub mod deb_dsc;
pub mod deb_install;
pub mod deb_repo;
pub mod recipe;
pub mod staged_pkg;
pub mod web;

/// Default for maximum number of levels to descend down dependencies tree.
pub const WALK_DEPTH: usize = 16;

/// Default remote package source, for recipes with build type = "remote".
/// This is overridable with mirrors config (via `cookbook.toml`).
pub const REMOTE_PKG_SOURCE: &str = "https://static.redox-os.org/pkg";

pub fn is_carcinization() -> bool {
    cfg!(target_os = "redox")
}

pub fn cross_target() -> Option<String> {
    std::env::var("COOKBOOK_CROSS_TARGET")
        .ok()
        .filter(|s| !s.is_empty())
}

// Errors

use std::fmt::Display;
use std::io;
use std::path::PathBuf;
use std::process::{Command, ExitStatus};

/// Error types used through cookbook.
///
/// When writing IO context, don't use "Failed at XXX". Look at impl Display for suitable word to use.
#[derive(Debug)]
pub enum Error {
    Io {
        source: io::Error,
        path: Option<PathBuf>,
        context: &'static str,
    },
    FileIo {
        source: io::Error,
        src: PathBuf,
        dst: PathBuf,
        context: &'static str,
    },
    // `Command` is by far the largest variant, at 200 bytes. Boxing it keeps
    // the whole enum small enough to be returned by value everywhere.
    Command(Box<Command>, ExitStatus),
    // The errors of the packaging crates are large, and they are the only
    // reason this enum grew past the size where returning it by value stops
    // being free. Boxing them keeps `Result` small everywhere.
    Package(Box<carcinization_pkg::PackageError>),
    PackageBackend(Box<carcinization_pkg::backend::Error>),
    Pkgar(Box<pkgar::Error>),
    Options(String),
    Other(String),
}

impl Error {
    pub fn from_last_io_error(context: &'static str) -> Error {
        wrap_io_err!(context)(io::Error::last_os_error())
    }
    pub fn from_io_error(err: io::Error, context: &'static str) -> Error {
        wrap_io_err!(context)(err)
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io {
                source,
                path,
                context,
            } => {
                if let Some(path) = path {
                    write!(f, "{context} failed at \"{}\": {}", path.display(), source)
                } else {
                    write!(f, "{context} failed: {}", source)
                }
            }
            Error::FileIo {
                source,
                src,
                dst,
                context,
            } => {
                write!(
                    f,
                    "{context} failed from \"{}\" to \"{}\": {}",
                    src.display(),
                    dst.display(),
                    source
                )
            }
            Error::Command(command, exit_status) => {
                write!(
                    f,
                    "Failed to run [{:?}]: exited with status {}",
                    command, exit_status
                )
            }
            Error::Package(package_error) => write!(f, "Package error: {}", package_error),
            Error::PackageBackend(package_error) => {
                write!(f, "Package backend error: {}", package_error)
            }
            Error::Pkgar(error) => write!(f, "Package archive error: {}", error),
            Error::Other(context) | Error::Options(context) => {
                write!(f, "{context}")
            }
        }
    }
}

macro_rules! wrap_io_err {
    ($context:expr) => {
        |source| crate::Error::Io {
            source,
            path: None,
            context: $context,
        }
    };
    ($path:expr, $context:expr) => {
        |source| crate::Error::Io {
            source,
            path: Some($path.to_path_buf()),
            context: $context,
        }
    };
    ($src:expr, $dst:expr, $context:expr) => {
        |source| crate::Error::FileIo {
            source,
            src: $src.to_path_buf(),
            dst: $dst.to_path_buf(),
            context: $context,
        }
    };
}

macro_rules! wrap_other_err {
    ($($arg:tt)*) => {
        || crate::Error::Other(format!($($arg)*))
    };
}

macro_rules! bail_other_err {
    ($($arg:tt)*) => {
        return Err(crate::Error::Other(format!($($arg)*)))
    };
}

impl From<&'static str> for Error {
    fn from(value: &'static str) -> Self {
        Error::Other(value.to_string())
    }
}
impl From<String> for Error {
    fn from(value: String) -> Self {
        Error::Other(value)
    }
}

impl From<Error> for String {
    fn from(val: Error) -> Self {
        format!("{}", val)
    }
}

impl From<carcinization_pkg::PackageError> for Error {
    fn from(value: carcinization_pkg::PackageError) -> Self {
        Error::Package(Box::new(value))
    }
}

impl From<carcinization_pkg::backend::Error> for Error {
    fn from(value: carcinization_pkg::backend::Error) -> Self {
        match value {
            carcinization_pkg::backend::Error::IO(source, path, context) => Error::Io {
                source,
                path: Some(path),
                context,
            },
            carcinization_pkg::backend::Error::Download(
                carcinization_pkg::net_backend::DownloadError::IO(error),
            ) => Error::Io {
                source: error,
                path: None,
                context: "Package network I/O",
            },
            error => Error::PackageBackend(Box::new(error)),
        }
    }
}

impl From<pkgar::Error> for Error {
    fn from(value: pkgar::Error) -> Self {
        match value {
            pkgar::Error::Io {
                source,
                path,
                context,
            } => Error::Io {
                source,
                path,
                context,
            },
            _ => Error::Pkgar(Box::new(value)),
        }
    }
}

impl From<pkgar_keys::Error> for Error {
    fn from(value: pkgar_keys::Error) -> Self {
        Error::Pkgar(Box::new(pkgar::Error::Keys(value)))
    }
}

impl From<walkdir::Error> for Error {
    fn from(value: walkdir::Error) -> Self {
        if value.io_error().is_some() {
            let path = value.path().map(|s| s.to_path_buf());
            Error::Io {
                source: value.into_io_error().unwrap(),
                path,
                context: "Walkdir error",
            }
        } else {
            wrap_other_err!(
                "Walkdir file system loop found at {:?}",
                value.path().map(|s| s.to_string_lossy().to_string()),
            )()
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) use wrap_io_err;

pub(crate) use wrap_other_err;

pub(crate) use bail_other_err;

pub(crate) use cook::pty::log_to_pty;
