use serde_json::{Value, json};

/// Build a minimal OCI runtime spec for a container.
///
/// The root path is relative to the bundle: containerd's shim mounts the
/// snapshot we prepared into `<bundle>/rootfs` before starting the container.
///
/// `cgroups_path` is the leaf cgroup runc puts the container in. It stays
/// unconfigured: resource limits belong to the pod cgroup above it, see
/// [`crate::cgroup`].
pub fn spec(
    hostname: &str,
    args: &[String],
    env: &[String],
    cwd: &str,
    cgroups_path: &str,
) -> Value {
    json!({
        "ociVersion": "1.0.2",
        "process": {
            "terminal": false,
            "user": { "uid": 0, "gid": 0 },
            "args": args,
            "env": env,
            "cwd": if cwd.is_empty() { "/" } else { cwd },
            "noNewPrivileges": true,
        },
        "root": { "path": "rootfs", "readonly": false },
        "hostname": hostname,
        "mounts": [
            { "destination": "/proc", "type": "proc", "source": "proc" },
            {
                "destination": "/dev",
                "type": "tmpfs",
                "source": "tmpfs",
                "options": ["nosuid", "strictatime", "mode=755", "size=65536k"],
            },
            {
                "destination": "/dev/pts",
                "type": "devpts",
                "source": "devpts",
                "options": ["nosuid", "noexec", "newinstance", "ptmxmode=0666", "mode=0620", "gid=5"],
            },
            {
                "destination": "/dev/shm",
                "type": "tmpfs",
                "source": "shm",
                "options": ["nosuid", "noexec", "nodev", "mode=1777", "size=65536k"],
            },
            {
                "destination": "/dev/mqueue",
                "type": "mqueue",
                "source": "mqueue",
                "options": ["nosuid", "noexec", "nodev"],
            },
            {
                "destination": "/sys",
                "type": "sysfs",
                "source": "sysfs",
                "options": ["nosuid", "noexec", "nodev", "ro"],
            },
        ],
        "linux": {
            "cgroupsPath": cgroups_path,
            "namespaces": [
                { "type": "pid" },
                { "type": "ipc" },
                { "type": "uts" },
                { "type": "mount" },
                { "type": "network" },
            ],
            "maskedPaths": [
                "/proc/kcore",
                "/proc/latency_stats",
                "/proc/timer_list",
                "/proc/sched_debug",
                "/sys/firmware",
            ],
            "readonlyPaths": [
                "/proc/asound",
                "/proc/bus",
                "/proc/fs",
                "/proc/irq",
                "/proc/sys",
                "/proc/sysrq-trigger",
            ],
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec_with(cgroups_path: &str) -> Value {
        spec("c", &["/bin/sh".to_string()], &[], "/", cgroups_path)
    }

    #[test]
    fn test_the_container_runs_in_the_cgroup_it_was_given() {
        let spec = spec_with("/barenetes/default.web/default.web-app");

        assert_eq!(
            spec["linux"]["cgroupsPath"],
            "/barenetes/default.web/default.web-app"
        );
    }

    #[test]
    fn test_the_container_cgroup_carries_no_limit_of_its_own() {
        assert!(
            spec_with("/barenetes/default.web/default.web-app")["linux"]["resources"].is_null()
        );
    }

    #[test]
    fn test_the_cgroup_does_not_disturb_the_rest_of_the_spec() {
        let spec = spec_with("/barenetes/default.web/default.web-app");

        assert_eq!(spec["hostname"], "c");
        assert_eq!(spec["linux"]["namespaces"][0]["type"], "pid");
    }
}
