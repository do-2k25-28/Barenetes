use std::io;
use std::process::{Command, Stdio};

const IP: &str = "/usr/sbin/ip";
pub const BRIDGE_NAME: &str = "barenetes0";
const BRIDGE_ADDRESS: &str = "10.244.0.1/16";

pub fn ensure_bridge() -> io::Result<()> {
    if unsafe { libc::geteuid() } != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "CNI network setup requires root privileges",
        ));
    }

    if !succeeds(IP, &["link", "show", "dev", BRIDGE_NAME])?
        && !succeeds(IP, &["link", "add", "name", BRIDGE_NAME, "type", "bridge"])?
        && !succeeds(IP, &["link", "show", "dev", BRIDGE_NAME])?
    {
        return Err(io::Error::other("failed to create CNI bridge"));
    }

    run(
        IP,
        &["address", "replace", BRIDGE_ADDRESS, "dev", BRIDGE_NAME],
    )?;
    run(IP, &["link", "set", "dev", BRIDGE_NAME, "up"])
}

pub fn succeeds(program: &str, arguments: &[&str]) -> io::Result<bool> {
    Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
}

pub fn run(program: &str, arguments: &[&str]) -> io::Result<()> {
    if succeeds(program, arguments)? {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "network command failed: {program} {}",
            arguments.join(" ")
        )))
    }
}
