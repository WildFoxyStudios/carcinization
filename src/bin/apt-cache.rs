//! The `apt-cache` frontend for the Carcinization OS package manager.

fn main() -> std::process::ExitCode {
    cookbook::apt_cli::run()
}
