use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const TOOL_DIRECTORIES: &[&str] = &["/usr/sbin", "/sbin", "/usr/bin", "/bin"];

// Leaves room for the 50 bytes of VXLAN encapsulation.
const DEFAULT_MTU: u32 = 1450;

pub(crate) fn mtu() -> io::Result<u32> {
    let Some(value) = std::env::var("BARENETES_MTU")
        .ok()
        .filter(|value| !value.is_empty())
    else {
        return Ok(DEFAULT_MTU);
    };
    value
        .parse()
        .ok()
        .filter(|mtu| (576..=9000).contains(mtu))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "BARENETES_MTU must be between 576 and 9000",
            )
        })
}

pub(crate) fn succeeds(program: &str, arguments: &[&str]) -> io::Result<bool> {
    let mut command = Command::new(resolve(program)?);
    if program == "iptables" {
        command.args(["-w", "5"]);
    }
    command
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
}

pub(crate) fn run(program: &str, arguments: &[&str]) -> io::Result<()> {
    if succeeds(program, arguments)? {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "network command failed: {program} {}",
            arguments.join(" ")
        )))
    }
}

pub(crate) fn resolve(program: &str) -> io::Result<PathBuf> {
    let variable = format!("BARENETES_{}_BIN", program.to_uppercase());
    if let Some(value) = std::env::var_os(&variable) {
        let path = PathBuf::from(value);
        return if path.is_file() {
            Ok(path)
        } else {
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("{variable} does not point to an existing file"),
            ))
        };
    }
    TOOL_DIRECTORIES
        .iter()
        .map(|directory| Path::new(directory).join(program))
        .find(|path| path.is_file())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "{program} not found in {}; set {variable} to its absolute path",
                    TOOL_DIRECTORIES.join(", ")
                ),
            )
        })
}
