//! /proc/sys: a fixed table of sysctl files whose values are stored (and
//! read back) but mostly not acted on. Writes to `kernel/hostname` and
//! `kernel/domainname` change uname's names; `kernel/random/{boot_id,
//! uuid}` are generated. Directories are the path prefixes of the table.

use alloc::string::String;
use alloc::vec::Vec;
use spin::{Mutex, Once};

/// (path below /proc/sys, initial value).
const ENTRIES: &[(&str, &str)] = &[
    ("abi/vsyscall32", "1"),
    ("fs/file-max", "9223372036854775807"),
    ("fs/file-nr", "0\t0\t9223372036854775807"),
    ("fs/inotify/max_queued_events", "16384"),
    ("fs/inotify/max_user_instances", "128"),
    ("fs/inotify/max_user_watches", "65536"),
    ("fs/mount-max", "100000"),
    ("fs/nr_open", "1048576"),
    ("fs/pipe-max-size", "1048576"),
    ("fs/protected_fifos", "0"),
    ("fs/protected_hardlinks", "1"),
    ("fs/protected_regular", "0"),
    ("fs/protected_symlinks", "1"),
    ("fs/suid_dumpable", "0"),
    ("fs/verity/require_signatures", "0"),
    ("kernel/bpf_stats_enabled", "0"),
    ("kernel/cap_last_cap", "40"),
    ("kernel/core_pattern", "core"),
    ("kernel/core_pipe_limit", "0"),
    ("kernel/core_uses_pid", "0"),
    ("kernel/ctrl-alt-del", "0"),
    ("kernel/dmesg_restrict", "0"),
    ("kernel/domainname", ""),
    ("kernel/ftrace_dump_on_oops", "0"),
    ("kernel/hostname", ""),
    ("kernel/hung_task_timeout_secs", "120"),
    ("kernel/io_uring_disabled", "0"),
    ("kernel/kexec_load_disabled", "0"),
    ("kernel/keys/maxbytes", "20000"),
    ("kernel/keys/maxkeys", "200"),
    ("kernel/kptr_restrict", "0"),
    ("kernel/modprobe", "/sbin/modprobe"),
    ("kernel/modules_disabled", "0"),
    ("kernel/ngroups_max", "65536"),
    ("kernel/osrelease", "6.6.0-android-pc"),
    ("kernel/ostype", "Linux"),
    ("kernel/overflowgid", "65534"),
    ("kernel/overflowuid", "65534"),
    ("kernel/panic", "0"),
    ("kernel/panic_on_oops", "1"),
    ("kernel/perf_cpu_time_max_percent", "25"),
    ("kernel/perf_event_max_sample_rate", "100000"),
    ("kernel/perf_event_paranoid", "2"),
    ("kernel/perf_user_access", "0"),
    ("kernel/pid_max", "4194304"),
    ("kernel/printk", "4\t4\t1\t7"),
    ("kernel/printk_devkmsg", "on"),
    ("kernel/random/boot_id", ""),
    ("kernel/random/entropy_avail", "256"),
    ("kernel/random/poolsize", "256"),
    ("kernel/random/urandom_min_reseed_secs", "60"),
    ("kernel/random/uuid", ""),
    ("kernel/random/write_wakeup_threshold", "256"),
    ("kernel/randomize_va_space", "2"),
    ("kernel/sched_child_runs_first", "0"),
    ("kernel/sched_energy_aware", "1"),
    ("kernel/sched_latency_ns", "10000000"),
    ("kernel/sched_rt_period_us", "1000000"),
    ("kernel/sched_rt_runtime_us", "950000"),
    ("kernel/sched_schedstats", "0"),
    ("kernel/sched_tunable_scaling", "0"),
    ("kernel/sched_util_clamp_max", "1024"),
    ("kernel/sched_util_clamp_min", "1024"),
    ("kernel/sched_util_clamp_min_rt_default", "1024"),
    ("kernel/sched_wakeup_granularity_ns", "2000000"),
    ("kernel/shmmax", "18446744073692774399"),
    ("kernel/sysrq", "1"),
    ("kernel/threads-max", "65536"),
    ("kernel/timer_migration", "1"),
    ("kernel/unprivileged_bpf_disabled", "1"),
    ("kernel/usermodehelper/bset", "4294967295\t4294967295"),
    ("kernel/usermodehelper/inheritable", "4294967295\t4294967295"),
    ("kernel/version", "#1 SMP android-pc"),
    ("kernel/yama/ptrace_scope", "1"),
    ("net/core/optmem_max", "81920"),
    ("net/core/rmem_default", "212992"),
    ("net/core/rmem_max", "212992"),
    ("net/core/somaxconn", "4096"),
    ("net/core/wmem_default", "212992"),
    ("net/core/wmem_max", "212992"),
    ("net/ipv4/ping_group_range", "1\t0"),
    ("net/ipv4/tcp_default_init_rwnd", "60"),
    ("net/unix/max_dgram_qlen", "600"),
    ("vm/compaction_proactiveness", "20"),
    ("vm/dirty_background_ratio", "10"),
    ("vm/dirty_expire_centisecs", "3000"),
    ("vm/dirty_ratio", "20"),
    ("vm/dirty_writeback_centisecs", "500"),
    ("vm/drop_caches", "0"),
    ("vm/extra_free_kbytes", "0"),
    ("vm/lowmem_reserve_ratio", "256\t256\t32\t0\t0"),
    ("vm/max_map_count", "65530"),
    ("vm/min_free_kbytes", "8192"),
    ("vm/mmap_min_addr", "32768"),
    ("vm/mmap_rnd_bits", "28"),
    ("vm/mmap_rnd_compat_bits", "8"),
    ("vm/oom_dump_tasks", "1"),
    ("vm/oom_kill_allocating_task", "0"),
    ("vm/overcommit_memory", "0"),
    ("vm/overcommit_ratio", "50"),
    ("vm/page-cluster", "3"),
    ("vm/panic_on_oom", "0"),
    ("vm/swappiness", "60"),
    ("vm/unprivileged_userfaultfd", "0"),
    ("vm/vfs_cache_pressure", "100"),
    ("vm/watermark_boost_factor", "15000"),
    ("vm/watermark_scale_factor", "10"),
];

pub struct Node {
    pub path: String,
    /// Index of the value for files, `None` for directories.
    pub file: Option<usize>,
}

static NODES: Once<Vec<Node>> = Once::new();
static VALUES: Mutex<Vec<Vec<u8>>> = Mutex::new(Vec::new());

/// Every node; index 0 is /proc/sys itself (path "").
pub fn nodes() -> &'static [Node] {
    NODES.call_once(|| {
        let mut v = alloc::vec![Node { path: String::new(), file: None }];
        let mut vals = VALUES.lock();
        for (i, (path, init)) in ENTRIES.iter().enumerate() {
            let mut prefix = String::new();
            let comps: Vec<&str> = path.split('/').collect();
            for c in &comps[..comps.len() - 1] {
                if !prefix.is_empty() {
                    prefix.push('/');
                }
                prefix.push_str(c);
                if !v.iter().any(|n| n.path == prefix) {
                    v.push(Node { path: prefix.clone(), file: None });
                }
            }
            v.push(Node { path: (*path).into(), file: Some(i) });
            vals.push(init.as_bytes().to_vec());
        }
        v
    })
}

/// Index of the node at `path`.
pub fn find(path: &str) -> Option<usize> {
    nodes().iter().position(|n| n.path == path)
}

/// Direct children of directory `idx`: (name, index).
pub fn children(idx: usize) -> Vec<(String, usize)> {
    let dir = &nodes()[idx].path;
    nodes()
        .iter()
        .enumerate()
        .filter_map(|(i, n)| {
            let rest = if dir.is_empty() { n.path.as_str() } else { n.path.strip_prefix(dir.as_str())?.strip_prefix('/')? };
            (!rest.is_empty() && !rest.contains('/')).then(|| (rest.into(), i))
        })
        .collect()
}

pub fn read(idx: usize) -> Vec<u8> {
    let n = &nodes()[idx];
    let mut v = match n.path.as_str() {
        "kernel/random/boot_id" => return super::procfs::boot_id().into_bytes(),
        "kernel/random/uuid" => return super::procfs::random_uuid().into_bytes(),
        "kernel/hostname" => crate::syscall::sys::uts_names().0,
        "kernel/domainname" => crate::syscall::sys::uts_names().1,
        _ => VALUES.lock()[n.file.unwrap_or(0)].clone(),
    };
    v.push(b'\n');
    v
}

pub fn write(idx: usize, data: &[u8]) -> crate::errno::KResult<usize> {
    let n = &nodes()[idx];
    let file = n.file.ok_or(crate::errno::EISDIR)?;
    let mut v = data;
    while let Some((&last, rest)) = v.split_last() {
        if last == b'\n' || last == 0 {
            v = rest;
        } else {
            break;
        }
    }
    match n.path.as_str() {
        "kernel/random/boot_id" | "kernel/random/uuid" => return Err(crate::errno::EPERM),
        "kernel/hostname" => crate::syscall::sys::set_uts_name(v, false)?,
        "kernel/domainname" => crate::syscall::sys::set_uts_name(v, true)?,
        _ => {}
    }
    VALUES.lock()[file] = v.to_vec();
    Ok(data.len())
}
