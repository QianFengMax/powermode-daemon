//! 任务调度：CPU 亲和性、uclamp、nice。
//!
//! 复刻 uperf 的 task_scheduler.c 的核心部分。
//!
//! ## 做什么
//!
//! 对关键进程（前台、游戏）做三件事：
//!
//!   1. **CPU 亲和性**：绑到大核，减少跨簇调度开销
//!   2. **uclamp**：给个算力上下限
//!      - uclamp_min 抬高，保证低频时也不卡
//!      - uclamp_max 压低后台进程，让它抢不到太多算力
//!   3. **nice / 调度策略**：调整相对优先级
//!
//! ## 为什么只做「设置」不做「恢复」
//!
//! uperf 会记录每个线程的原始值并在退出时恢复。这需要遍历所有进程
//! 并维护一张大表，开销和复杂度都很高。
//!
//! 这里只对**我们自己启动的子进程**做设置（比如把 fast 名单里的进程
//! 提到大核），不做全局接管。进程退出时内核自动回收，不需要恢复。
//!
//! 这是刻意的取舍：全局接管需要常驻扫描全系统线程（250ms 一次），
//! 而收益主要是「后台不抢前台」——这件事 uclamp_max 就能做到，
//! 且可以由用户按需配置，不必默认接管所有进程。

use std::fs;

/// 设置某进程/线程的 CPU 亲和性。
///
/// `mask` 是 CPU 位图：bit 0 = CPU0。`tid` 为 0 表示设为 0（等同清除）。
///
/// 失败返回 Err，调用方应容忍——权限不足、进程已退出都可能导致失败。
pub fn set_affinity(pid: i32, mask: u64) -> Result<(), String> {
    if mask == 0 {
        return Err("空掩码".into());
    }
    let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    unsafe { libc::CPU_ZERO(&mut set) };
    for i in 0..64 {
        if (mask >> i) & 1 == 1 {
            unsafe { libc::CPU_SET(i, &mut set) };
        }
    }
    let rc = unsafe {
        libc::sched_setaffinity(pid, std::mem::size_of::<libc::cpu_set_t>(), &set)
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(())
}

/// 从 cpumask 字符串（如 "prime"、"performance"）解析出 CPU 位图。
///
/// 簇划分来自 config.json 的 sched.cpumask 段。这里用内核实际拓扑推断，
/// 不硬编码——不同 SoC 的簇大小不一样。
pub fn resolve_cpumask(name: &str) -> Option<u64> {
    let cpus = cluster_cpus();
    match name {
        "all" => Some(cpus.iter().fold(0u64, |m, &c| m | (1 << c))),
        "prime" => cpus.last().map(|&c| 1u64 << c),
        "efficiency" => cpus.first().map(|&c| 1u64 << c),
        "performance" => {
            // 中间那簇（如果有三簇）
            if cpus.len() >= 3 {
                Some(1u64 << cpus[1])
            } else {
                cpus.last().map(|&c| 1u64 << c)
            }
        }
        _ => None,
    }
}

/// 读内核拓扑，返回每个 policy 的第一个 CPU 编号。
///
/// 例如 sm8550 三簇：policy0 -> [0], policy3 -> [3], policy7 -> [7]
fn cluster_cpus() -> Vec<usize> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir("/sys/devices/system/cpu/cpufreq") else {
        return out;
    };

    let mut policies: Vec<_> = entries
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("policy"))
        .collect();
    policies.sort_by_key(|e| e.file_name());

    for e in policies {
        let Ok(raw) = fs::read_to_string(e.path().join("affected_cpus")) else {
            continue;
        };
        // "0" 或 "0 1 2" 这种格式，取第一个数字
        if let Some(first) = raw.split_whitespace().next() {
            if let Ok(n) = first.parse::<usize>() {
                out.push(n);
            }
        }
    }
    out
}

/// 返回 CPU 簇数量（用于日志与判断）。
pub fn cluster_count() -> usize {
    cluster_cpus().len()
}

/// 把进程绑到指定簇。
pub fn bind_to_cluster(pid: i32, cluster: &str) -> Result<(), String> {
    let Some(mask) = resolve_cpumask(cluster) else {
        return Err(format!("未知的簇: {cluster}"));
    };
    set_affinity(pid, mask)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpumask_names_resolve() {
        // 至少 all 应该能解析（有 CPU 就有）
        let all = resolve_cpumask("all");
        assert!(all.is_some(), "all 应该能解析");
        let m = all.unwrap();
        assert!(m != 0, "all 掩码不应为空");
    }

    #[test]
    fn unknown_cluster_rejected() {
        assert!(resolve_cpumask("nonexistent").is_none());
    }
}
