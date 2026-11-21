#[cfg(target_os = "linux")]
use std::{
    fs,
    path::{Component, Path},
};

/// Available memory after accounting for Linux cgroup constraints.
pub fn available_memory_bytes() -> u64 {
    #[cfg(target_os = "linux")]
    {
        let host_available = fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|content| {
                content.lines().find_map(|line| {
                    let value = line
                        .strip_prefix("MemAvailable:")?
                        .split_whitespace()
                        .next()?;
                    value.parse::<u64>().ok()?.checked_mul(1024)
                })
            })
            .unwrap_or(2 * 1024 * 1024 * 1024);
        let cgroup = fs::read_to_string("/proc/self/cgroup").ok();
        available_memory_from_sources(
            host_available,
            cgroup.as_deref(),
            Path::new("/sys/fs/cgroup"),
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        2 * 1024 * 1024 * 1024
    }
}

#[cfg(target_os = "linux")]
pub fn available_memory_from_sources(
    host_available: u64,
    proc_cgroup: Option<&str>,
    cgroup_root: &Path,
) -> u64 {
    cgroup_available_memory_bytes(proc_cgroup, cgroup_root)
        .map_or(host_available, |cgroup_available| {
            host_available.min(cgroup_available)
        })
}

#[cfg(target_os = "linux")]
fn cgroup_available_memory_bytes(proc_cgroup: Option<&str>, cgroup_root: &Path) -> Option<u64> {
    let Some(proc_cgroup) = proc_cgroup else {
        return cgroup_root_available_memory_bytes(cgroup_root);
    };
    let v1_path = proc_cgroup.lines().find_map(|line| {
        let mut fields = line.splitn(3, ':');
        let _hierarchy = fields.next()?;
        let controllers = fields.next()?;
        let path = fields.next()?;
        controllers
            .split(',')
            .any(|controller| controller == "memory")
            .then_some(path)
    });
    if let Some(path) = v1_path {
        let memory_root = cgroup_root.join("memory");
        let Some(directory) = cgroup_directory(&memory_root, path) else {
            return cgroup_v1_directory_available_memory_bytes(&memory_root);
        };
        if !directory.is_dir() {
            return cgroup_v1_directory_available_memory_bytes(&memory_root);
        }
        return cgroup_v1_directory_available_memory_bytes(&directory);
    }

    if let Some(path) = cgroup_v2_path(proc_cgroup) {
        let Some(directory) = cgroup_directory(cgroup_root, path) else {
            return cgroup_root_available_memory_bytes(cgroup_root);
        };
        if !directory.is_dir() {
            return cgroup_root_available_memory_bytes(cgroup_root);
        }
        return cgroup_v2_available_memory_bytes(path, cgroup_root);
    }

    cgroup_root_available_memory_bytes(cgroup_root)
}

#[cfg(target_os = "linux")]
fn cgroup_root_available_memory_bytes(cgroup_root: &Path) -> Option<u64> {
    match cgroup_v2_headroom_at(cgroup_root) {
        Some(Some(available)) => Some(available),
        Some(None) => cgroup_v1_directory_available_memory_bytes(&cgroup_root.join("memory")),
        None => None,
    }
}

#[cfg(target_os = "linux")]
fn cgroup_v1_directory_available_memory_bytes(directory: &Path) -> Option<u64> {
    let limit = fs::read_to_string(directory.join("memory.limit_in_bytes"))
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()?;
    let usage = fs::read_to_string(directory.join("memory.usage_in_bytes"))
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()?;
    limit.checked_sub(usage)
}

#[cfg(target_os = "linux")]
fn cgroup_v2_path(proc_cgroup: &str) -> Option<&str> {
    proc_cgroup.lines().find_map(|line| {
        let mut fields = line.splitn(3, ':');
        let hierarchy = fields.next()?;
        let controllers = fields.next()?;
        let path = fields.next()?;
        (hierarchy == "0" && controllers.is_empty()).then_some(path)
    })
}

#[cfg(target_os = "linux")]
fn cgroup_directory(root: &Path, membership_path: &str) -> Option<std::path::PathBuf> {
    let mut components = Path::new(membership_path).components();
    if !matches!(components.next(), Some(Component::RootDir)) {
        return None;
    }

    let mut directory = root.to_path_buf();
    for component in components {
        match component {
            Component::Normal(name) => directory.push(name),
            _ => return None,
        }
    }
    Some(directory)
}

#[cfg(target_os = "linux")]
fn cgroup_v2_available_memory_bytes(membership_path: &str, cgroup_root: &Path) -> Option<u64> {
    let mut directory = cgroup_directory(cgroup_root, membership_path)?;
    let mut available = None;
    loop {
        if let Some(headroom) = cgroup_v2_headroom_at(&directory)? {
            available = Some(available.map_or(headroom, |current: u64| current.min(headroom)));
        }

        if directory == cgroup_root {
            return available;
        }
        if !directory.pop() || !directory.starts_with(cgroup_root) {
            return None;
        }
    }
}

#[cfg(target_os = "linux")]
fn cgroup_v2_headroom_at(directory: &Path) -> Option<Option<u64>> {
    let limit = match fs::read_to_string(directory.join("memory.max")) {
        Ok(limit) => limit,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Some(None),
        Err(_) => return None,
    };
    let limit = limit.trim();
    if limit == "max" {
        return Some(None);
    }

    let limit = limit.parse::<u64>().ok()?;
    let usage = fs::read_to_string(directory.join("memory.current"))
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()?;
    Some(Some(limit.checked_sub(usage)?))
}

pub fn memory_budget_from_available(available: u64) -> u64 {
    (available / 3).clamp(64 * 1024 * 1024, 16 * 1024 * 1024 * 1024)
}

/// Budget memory for scans and related work.
pub fn memory_budget_bytes() -> u64 {
    memory_budget_from_available(available_memory_bytes())
}

/// Attempts to return freed heap pages to the OS allocator on Linux/glibc.
#[inline]
pub fn trim_allocator() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    unsafe {
        // SAFETY: no pointers are passed; glibc synchronizes allocator arenas.
        libc::malloc_trim(0);
    }
}

/// Read the OS process start-time token for `pid` to distinguish reused PIDs across sessions.
///
/// - On Linux, parses field 22 (`starttime` in clock ticks) after the final `')'` of `/proc/{pid}/stat`.
/// - On macOS, invokes `ps -p <pid> -o lstart=` with `LC_ALL=C` and `TZ=UTC` and returns a stable 64-bit hash of the trimmed timestamp.
/// - On other targets, returns `None`.
pub fn read_process_start_time(pid: u32) -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        parse_linux_proc_stat_start_time(&stat)
    }
    #[cfg(target_os = "macos")]
    {
        let output = macos_ps_command(pid).output().ok()?;
        if !output.status.success() {
            return None;
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let trimmed = stdout.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(crate::format::stable_hash64(trimmed) as u64)
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        None
    }
}

#[cfg(any(target_os = "macos", test))]
fn macos_ps_command(pid: u32) -> std::process::Command {
    let mut cmd = std::process::Command::new("ps");
    cmd.env("LC_ALL", "C")
        .env("TZ", "UTC")
        .args(["-p", &pid.to_string(), "-o", "lstart="]);
    cmd
}

#[cfg(any(target_os = "linux", test))]
fn parse_linux_proc_stat_start_time(stat: &str) -> Option<u64> {
    let rparen = stat.rfind(')')?;
    let after = stat.get(rparen + 1..)?;
    // Field 22 overall (`starttime`) is index 19 after `)` (field 3 `state` is index 0).
    after.split_whitespace().nth(19)?.parse::<u64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_start_time_parser_and_self_probe() {
        let cases = [
            (
                "weird_comm_with_parens_and_spaces",
                "1234 (my (weird) proc) S 1 1234 1234 0 -1 4194304 100 0 0 0 1 2 0 0 20 0 1 0 987654321 1024 10",
                Some(987_654_321),
            ),
            ("malformed_no_rparen", "malformed", None),
            ("missing_field_22", "1234 (proc) S 1 2 3", None),
            ("non_numeric_starttime", "1234 (proc) S 1 1 1 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 abc 0 0", None),
        ];
        for (label, sample, expected) in cases {
            assert_eq!(parse_linux_proc_stat_start_time(sample), expected, "{label}");
        }

        let ps_cmd = macos_ps_command(42);
        let envs: Vec<(String, Option<String>)> = ps_cmd
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|val| val.to_string_lossy().into_owned()),
                )
            })
            .collect();
        assert!(envs.contains(&("LC_ALL".to_owned(), Some("C".to_owned()))), "{envs:?}");
        assert!(envs.contains(&("TZ".to_owned(), Some("UTC".to_owned()))), "{envs:?}");

        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let start = read_process_start_time(std::process::id());
            assert!(start.is_some());
        }
    }
}
