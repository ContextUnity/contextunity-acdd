#[cfg(target_os = "linux")]
use std::{
    fs,
    path::{Component, Path},
};

/// Available memory after accounting for Linux cgroup constraints.
fn available_memory_bytes() -> u64 {
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
fn available_memory_from_sources(
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

fn memory_budget_from_available(available: u64) -> u64 {
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

/// Spawn `program` with `args` inside `cwd`, inheriting stdio, applying `env_updates`,
/// and returning the child's exit code (`128 + signal` on Unix signals).
pub fn run_inherited_in_dir(
    program: &str,
    args: &[String],
    cwd: &std::path::Path,
    env_updates: &[(&str, Option<&str>)],
) -> std::io::Result<i32> {
    let mut child = std::process::Command::new(program);
    child
        .args(args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit());
    for (key, value) in env_updates {
        match value {
            Some(val) => {
                child.env(key, val);
            }
            None => {
                child.env_remove(key);
            }
        }
    }
    let status = child.status()?;
    if let Some(code) = status.code() {
        return Ok(code);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return Ok(128 + sig);
        }
    }
    Ok(1)
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
            assert_eq!(
                parse_linux_proc_stat_start_time(sample),
                expected,
                "{label}"
            );
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
        assert!(
            envs.contains(&("LC_ALL".to_owned(), Some("C".to_owned()))),
            "{envs:?}"
        );
        assert!(
            envs.contains(&("TZ".to_owned(), Some("UTC".to_owned()))),
            "{envs:?}"
        );

        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let start = read_process_start_time(std::process::id());
            assert!(start.is_some());
        }
    }

    #[test]
    fn memory_budget_uses_a_64_mib_floor_and_16_gib_cap() {
        assert_eq!(memory_budget_from_available(0), 64 * 1024 * 1024);
        assert_eq!(
            memory_budget_from_available(768 * 1024 * 1024),
            256 * 1024 * 1024
        );
        assert_eq!(
            memory_budget_from_available(64 * 1024 * 1024 * 1024),
            16 * 1024 * 1024 * 1024
        );
    }

    #[cfg(target_os = "linux")]
    struct CgroupFixture(crate::fs::ScopedTempDir);

    #[cfg(target_os = "linux")]
    impl CgroupFixture {
        fn new() -> Self {
            Self(crate::fs::ScopedTempDir::create("acdd_cgroup_memory").unwrap())
        }

        fn write(&self, path: &str, content: &str) {
            let path = self.root().join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }

        fn root(&self) -> std::path::PathBuf {
            self.0.path().join("cgroup")
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cgroup_available_memory() {
        struct Case {
            case_id: &'static str,
            host: u64,
            membership: Option<&'static str>,
            files: &'static [(&'static str, &'static str)],
            expected: u64,
        }
        let nested = &[
            ("pod/memory.max", "400\n"),
            ("pod/memory.current", "200\n"),
            ("pod/container/memory.max", "max\n"),
        ];
        let root_limit = &[("memory.max", "500\n"), ("memory.current", "200\n")];
        let fixture = CgroupFixture::new();
        for case in [
            Case {
                case_id: "nested_cgroup_v2_uses_tightest_limit_and_host_headroom::nested",
                host: 500,
                membership: Some("0::/pod/container\n"),
                files: nested,
                expected: 200,
            },
            Case {
                case_id: "nested_cgroup_v2_uses_tightest_limit_and_host_headroom::host",
                host: 100,
                membership: Some("0::/pod/container\n"),
                files: nested,
                expected: 100,
            },
            Case {
                case_id: "unavailable_or_unresolvable_membership_uses_safe_cgroup_root::absent",
                host: 600,
                membership: None,
                files: root_limit,
                expected: 300,
            },
            Case {
                case_id: "unavailable_or_unresolvable_membership_uses_safe_cgroup_root::unresolved",
                host: 600,
                membership: Some("0::/namespace/path/not-mounted\n"),
                files: root_limit,
                expected: 300,
            },
            Case {
                case_id: "unlimited_or_over_limit_cgroups_fall_back_to_host_memory::unlimited",
                host: 321,
                membership: Some("0::/pod/container\n"),
                files: &[
                    ("memory.max", "max\n"),
                    ("pod/memory.max", "max\n"),
                    ("pod/container/memory.max", "max\n"),
                ],
                expected: 321,
            },
            Case {
                case_id: "unlimited_or_over_limit_cgroups_fall_back_to_host_memory::over_limit",
                host: 321,
                membership: Some("0::/pod/container\n"),
                files: &[
                    ("memory.max", "200\n"),
                    ("memory.current", "100\n"),
                    ("pod/memory.max", "100\n"),
                    ("pod/memory.current", "101\n"),
                    ("pod/container/memory.max", "max\n"),
                ],
                expected: 321,
            },
            Case {
                case_id: "malformed_or_unsafe_cgroup_paths_fall_back_to_host_memory::malformed",
                host: 321,
                membership: Some("0::/bad\n"),
                files: &[("bad/memory.max", "not-a-limit\n")],
                expected: 321,
            },
            Case {
                case_id: "malformed_or_unsafe_cgroup_paths_fall_back_to_host_memory::traversal",
                host: 321,
                membership: Some("0::/../host\n"),
                files: &[("memory.max", "max\n")],
                expected: 321,
            },
            Case {
                case_id: "cgroup_v1_over_limit_falls_back_without_subtraction_panic::available",
                host: 321,
                membership: Some("5:cpu,memory:/\n"),
                files: &[
                    ("memory/memory.limit_in_bytes", "100\n"),
                    ("memory/memory.usage_in_bytes", "40\n"),
                ],
                expected: 60,
            },
            Case {
                case_id: "cgroup_v1_over_limit_falls_back_without_subtraction_panic::hybrid",
                host: 321,
                membership: Some("0::/nested\n7:memory:/nested\n"),
                files: &[
                    ("memory/memory.limit_in_bytes", "100\n"),
                    ("memory/memory.usage_in_bytes", "40\n"),
                    ("memory/nested/memory.limit_in_bytes", "100\n"),
                    ("memory/nested/memory.usage_in_bytes", "40\n"),
                ],
                expected: 60,
            },
            Case {
                case_id: "cgroup_v1_over_limit_falls_back_without_subtraction_panic::over_limit",
                host: 321,
                membership: Some("5:cpu,memory:/\n"),
                files: &[
                    ("memory/memory.limit_in_bytes", "100\n"),
                    ("memory/memory.usage_in_bytes", "101\n"),
                ],
                expected: 321,
            },
        ] {
            if fixture.root().exists() {
                fs::remove_dir_all(fixture.root()).unwrap();
            }
            for (path, content) in case.files {
                fixture.write(path, content);
            }
            if case.membership == Some("0::/../host\n") {
                fs::write(fixture.0.path().join("memory.max"), "max\n").unwrap();
                let escaped = fixture.0.path().join("host");
                fs::create_dir_all(&escaped).unwrap();
                fs::write(escaped.join("memory.max"), "100\n").unwrap();
                fs::write(escaped.join("memory.current"), "90\n").unwrap();
            }
            if case.membership == Some("0::/nested\n7:memory:/nested\n") {
                fs::create_dir_all(fixture.root().join("nested")).unwrap();
            }
            assert_eq!(
                available_memory_from_sources(case.host, case.membership, &fixture.root()),
                case.expected,
                "{}",
                case.case_id,
            );
        }
    }
}
