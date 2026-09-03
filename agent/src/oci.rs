use proto::shared::v1::Resources;
use serde_json::{Value, json};

/// CFS period the quota is expressed against, in microseconds. One full period
/// of quota means one entire core, so 1000 mCPU maps to `CPU_PERIOD`.
const CPU_PERIOD: i64 = 100_000;

/// Smallest cfs_quota_us the kernel accepts for a non-infinite quota; anything
/// below this is rejected with EINVAL by both cgroup v1 and v2.
const CPU_MIN_QUOTA: i64 = 1000;

/// Build a minimal OCI runtime spec for a container.
///
/// The root path is relative to the bundle: containerd's shim mounts the
/// snapshot we prepared into `<bundle>/rootfs` before starting the container.
///
/// `limits` are the resource limits of the pod the container belongs to; runc
/// turns them into the cgroup of the container.
pub fn spec(
    hostname: &str,
    args: &[String],
    env: &[String],
    cwd: &str,
    limits: Option<&Resources>,
) -> Value {
    let mut spec = json!({
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
    });

    if let Some(resources) = resources(limits) {
        spec["linux"]["resources"] = resources;
    }

    spec
}

/// Translate pod limits into the `linux.resources` section of the spec.
///
/// A missing or non-positive value means "unlimited" and is left out, so runc
/// keeps the cgroup default for it.
fn resources(limits: Option<&Resources>) -> Option<Value> {
    let limits = limits?;
    let mut resources = json!({});

    if limits.cpu > 0 {
        let quota = (i64::from(limits.cpu) * CPU_PERIOD / 1000).max(CPU_MIN_QUOTA);
        resources["cpu"] = json!({
            "period": CPU_PERIOD,
            "quota": quota,
        });
    }

    if limits.memory > 0 {
        let bytes = i64::from(limits.memory) * 1024 * 1024;
        // Capping swap at the memory limit too, otherwise the container walks
        // around its limit by swapping.
        resources["memory"] = json!({ "limit": bytes, "swap": bytes });
    }

    match resources.as_object() {
        Some(fields) if !fields.is_empty() => Some(resources),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits(cpu: i32, memory: i32) -> Resources {
        Resources { cpu, memory }
    }

    fn spec_with(limits: Option<&Resources>) -> Value {
        spec("c", &["/bin/sh".to_string()], &[], "/", limits)
    }

    #[test]
    fn test_no_limits_leaves_resources_out() {
        assert!(spec_with(None)["linux"]["resources"].is_null());
    }

    #[test]
    fn test_zeroed_limits_leave_resources_out() {
        assert!(spec_with(Some(&limits(0, 0)))["linux"]["resources"].is_null());
    }

    #[test]
    fn test_one_core_is_a_full_period_of_quota() {
        let spec = spec_with(Some(&limits(1000, 0)));
        let cpu = &spec["linux"]["resources"]["cpu"];

        assert_eq!(cpu["period"], CPU_PERIOD);
        assert_eq!(cpu["quota"], CPU_PERIOD);
    }

    #[test]
    fn test_millicpus_become_a_fraction_of_a_period() {
        let spec = spec_with(Some(&limits(250, 0)));

        assert_eq!(spec["linux"]["resources"]["cpu"]["quota"], CPU_PERIOD / 4);
    }

    #[test]
    fn test_small_cpu_limits_are_clamped_to_the_kernel_minimum() {
        let spec = spec_with(Some(&limits(5, 0)));

        assert_eq!(spec["linux"]["resources"]["cpu"]["quota"], CPU_MIN_QUOTA);
    }

    #[test]
    fn test_memory_is_converted_to_bytes() {
        let spec = spec_with(Some(&limits(0, 512)));
        let memory = &spec["linux"]["resources"]["memory"];

        assert_eq!(memory["limit"], 512 * 1024 * 1024);
        // Swap is capped at the memory limit, not left unlimited.
        assert_eq!(memory["swap"], 512 * 1024 * 1024);
    }

    #[test]
    fn test_only_the_set_limits_are_kept() {
        let spec = spec_with(Some(&limits(500, 0)));
        let resources = &spec["linux"]["resources"];

        assert!(resources["cpu"].is_object());
        assert!(resources["memory"].is_null());
    }

    #[test]
    fn test_limits_do_not_disturb_the_rest_of_the_spec() {
        let spec = spec_with(Some(&limits(1000, 128)));

        assert_eq!(spec["hostname"], "c");
        assert_eq!(spec["linux"]["namespaces"][0]["type"], "pid");
    }
}
