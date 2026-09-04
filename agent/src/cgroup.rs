//! Pod-level cgroups.
//!
//! Limits are declared for the whole pod, so they are enforced on a single
//! cgroup per pod: `/sys/fs/cgroup/barenetes/<pod-id>`. Every container of the
//! pod runs in its own leaf cgroup underneath it and carries no limit of its
//! own. The kernel accounts a cgroup for all of its descendants, so the
//! containers share the pod budget instead of each receiving a full copy of it.
//!
//! Only the cgroup v2 unified hierarchy is handled, which is what current
//! distributions mount.

use std::io;
use std::path::{Path, PathBuf};

use proto::shared::v1::Resources;

/// Mount point of the unified hierarchy.
const CGROUP_ROOT: &str = "/sys/fs/cgroup";

/// Pod cgroups all live under this one, so they stay together and apart from
/// whatever else the host runs.
const SLICE: &str = "barenetes";

/// CFS period the quota is expressed against, in microseconds. One full period
/// of quota means one entire core, so 1000 mCPU maps to `CPU_PERIOD`.
const CPU_PERIOD: i64 = 100_000;

/// Smallest quota the kernel accepts for a non-infinite `cpu.max`; anything
/// below this is rejected with EINVAL.
const CPU_MIN_QUOTA: i64 = 1000;

/// What an unlimited resource reads as in every cgroup file that takes one.
const MAX: &str = "max";

/// Controllers a pod cgroup needs. A cgroup can only use the ones its parent
/// hands down, so they are enabled on every ancestor of the pod cgroup.
const CONTROLLERS: &str = "+cpu +memory";

/// Path of the pod cgroup relative to the cgroup root, in the form the OCI
/// runtime spec expects in `linux.cgroupsPath`.
pub fn pod_path(pod_id: &str) -> String {
    format!("/{SLICE}/{pod_id}")
}

/// Create the cgroup of a pod and write `limits` to it.
///
/// Containers started afterwards under `pod_path(pod_id)` are capped by it, all
/// of them together.
pub fn create_pod(pod_id: &str, limits: Option<&Resources>) -> io::Result<()> {
    let pod = pod_dir(pod_id)?;
    let limits = limits.filter(|limits| limits.cpu > 0 || limits.memory > 0);

    // Nothing to enforce, and no cgroup left over from a previous revision of
    // the pod to clear: leave the hierarchy alone, runc creates what the
    // container leaves need.
    if limits.is_none() && !pod.exists() {
        return Ok(());
    }

    let root = Path::new(CGROUP_ROOT);
    if !root.join("cgroup.controllers").exists() {
        return Err(io::Error::other(format!(
            "resource limits need a cgroup v2 hierarchy mounted at {CGROUP_ROOT}"
        )));
    }

    let slice = root.join(SLICE);
    mkdir(&slice)?;
    delegate(root)?;
    delegate(&slice)?;
    mkdir(&pod)?;

    for (file, value) in limit_files(limits) {
        write(pod.join(file), value)?;
    }

    Ok(())
}

/// Remove the cgroup of a pod, once its containers are gone.
pub fn remove_pod(pod_id: &str) -> io::Result<()> {
    let pod = pod_dir(pod_id)?;

    // runc removes the leaf cgroup of every container it deletes, but a shim
    // that died mid-delete can leave an empty one behind, and an empty leftover
    // is enough to keep the pod cgroup from going away.
    if let Ok(entries) = std::fs::read_dir(&pod) {
        for leaf in entries.flatten().filter(|leaf| leaf.path().is_dir()) {
            std::fs::remove_dir(leaf.path()).ok();
        }
    }

    match std::fs::remove_dir(&pod) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

/// Directory of the pod cgroup on the host.
///
/// Pod ids reach us from the API, so they are kept from walking out of the
/// slice and turning a limit into a write anywhere under `/sys/fs/cgroup`.
fn pod_dir(pod_id: &str) -> io::Result<PathBuf> {
    if pod_id.is_empty() || pod_id.contains('/') || pod_id.trim_matches('.').is_empty() {
        return Err(io::Error::other(format!(
            "{pod_id:?} is not a usable cgroup name"
        )));
    }
    Ok(Path::new(CGROUP_ROOT).join(SLICE).join(pod_id))
}

/// Hand the controllers down to the children of `cgroup`.
fn delegate(cgroup: &Path) -> io::Result<()> {
    write(
        cgroup.join("cgroup.subtree_control"),
        CONTROLLERS.to_string(),
    )
}

/// The limit files of a pod cgroup, and what `limits` puts in them.
///
/// A limit that is not set is written as `max` rather than left alone. Every
/// apply then lands the pod on exactly the limits it declares, whatever the
/// cgroup of its previous revision was holding.
fn limit_files(limits: Option<&Resources>) -> [(&'static str, String); 3] {
    let cpu = limits.map(|limits| limits.cpu).filter(|cpu| *cpu > 0);
    let memory = limits
        .map(|limits| limits.memory)
        .filter(|memory| *memory > 0);

    [
        (
            "cpu.max",
            cpu.map_or_else(|| format!("max {CPU_PERIOD}"), cpu_max),
        ),
        (
            "memory.max",
            memory.map_or_else(
                || MAX.to_string(),
                |memory| memory_bytes(memory).to_string(),
            ),
        ),
        // Swap is pinned to 0 under a memory limit, otherwise the pod walks
        // around the limit by swapping.
        (
            "memory.swap.max",
            memory.map_or_else(|| MAX.to_string(), |_| "0".to_string()),
        ),
    ]
}

/// `cpu.max` is a quota over a period: the cgroup gets `quota` microseconds of
/// CPU time out of every `period` microseconds.
fn cpu_max(cpu: i32) -> String {
    let quota = (i64::from(cpu) * CPU_PERIOD / 1000).max(CPU_MIN_QUOTA);
    format!("{quota} {CPU_PERIOD}")
}

fn memory_bytes(memory: i32) -> i64 {
    i64::from(memory) * 1024 * 1024
}

/// Both of these name the directory they failed on: the usual failures here
/// (a read-only cgroupfs in a container, a hardened unit, a non-root agent)
/// are impossible to place from a bare errno.
fn mkdir(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)
        .map_err(|error| io::Error::new(error.kind(), format!("{}: {error}", dir.display())))
}

fn write(file: PathBuf, value: String) -> io::Result<()> {
    std::fs::write(&file, value)
        .map_err(|error| io::Error::new(error.kind(), format!("{}: {error}", file.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_containers_sit_under_the_cgroup_of_their_pod() {
        assert_eq!(pod_path("default.web"), "/barenetes/default.web");
    }

    #[test]
    fn test_one_core_is_a_full_period_of_quota() {
        assert_eq!(cpu_max(1000), format!("{CPU_PERIOD} {CPU_PERIOD}"));
    }

    #[test]
    fn test_millicpus_become_a_fraction_of_a_period() {
        assert_eq!(cpu_max(250), format!("{} {CPU_PERIOD}", CPU_PERIOD / 4));
    }

    #[test]
    fn test_small_cpu_limits_are_clamped_to_the_kernel_minimum() {
        assert_eq!(cpu_max(5), format!("{CPU_MIN_QUOTA} {CPU_PERIOD}"));
    }

    fn value_of(limits: Option<&Resources>, file: &str) -> String {
        limit_files(limits)
            .into_iter()
            .find(|(name, _)| *name == file)
            .expect("no such limit file")
            .1
    }

    #[test]
    fn test_a_pod_without_limits_resets_every_file_to_max() {
        assert_eq!(value_of(None, "cpu.max"), format!("max {CPU_PERIOD}"));
        assert_eq!(value_of(None, "memory.max"), "max");
        assert_eq!(value_of(None, "memory.swap.max"), "max");
    }

    #[test]
    fn test_dropping_one_limit_leaves_the_other_alone() {
        let cpu_only = Resources {
            cpu: 500,
            memory: 0,
        };

        assert_eq!(
            value_of(Some(&cpu_only), "cpu.max"),
            format!("50000 {CPU_PERIOD}")
        );
        assert_eq!(value_of(Some(&cpu_only), "memory.max"), "max");
    }

    #[test]
    fn test_a_memory_limit_pins_swap_to_zero() {
        let memory_only = Resources {
            cpu: 0,
            memory: 512,
        };

        assert_eq!(value_of(Some(&memory_only), "memory.swap.max"), "0");
        assert_eq!(value_of(None, "memory.swap.max"), "max");
    }

    #[test]
    fn test_memory_is_converted_to_bytes() {
        assert_eq!(memory_bytes(512), 512 * 1024 * 1024);
    }

    #[test]
    fn test_a_pod_without_limits_and_without_a_cgroup_is_left_alone() {
        // Writes nothing, so it stays a no-op even on a host with a cgroup v2
        // hierarchy this test cannot write to.
        let unknown = "test.no-such-pod";

        assert!(create_pod(unknown, None).is_ok());
        assert!(create_pod(unknown, Some(&Resources { cpu: 0, memory: 0 })).is_ok());
    }

    #[test]
    fn test_pod_ids_cannot_escape_the_slice() {
        assert!(pod_dir("../../devices").is_err());
        assert!(pod_dir("..").is_err());
        assert!(pod_dir("").is_err());
        assert!(pod_dir("default.web").is_ok());
    }
}
