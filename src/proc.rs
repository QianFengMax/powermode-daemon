//! 进程扫描与负载检测。
//!
//! 复刻 uperf 的 game_scanner.c + heavyload_detector.c 的核心部分。
//!
//! 用途：找出当前最该被照顾的进程——
//!   1. 匹配 fast 名单的应用（游戏、重负载）→ 直接进性能档
//!   2. 否则统计总 CPU 占用，按阈值决定档位
//!
//! 采样方式：读 /proc/stat 算 CPU 时间差，读 /proc/[pid]/stat 算单进程占用。
//! 这是 uperf 的做法，比调 cpufreq 的负载统计更轻。

use std::fs;

/// 一次采样得到的 CPU 使用率快照。
#[derive(Debug, Clone, Copy, Default)]
pub struct CpuSample {
    pub total: u64,
    pub idle: u64,
}

impl CpuSample {
    /// 读 /proc/stat 第一行（整体 CPU）。
    pub fn read() -> Self {
        let Ok(text) = fs::read_to_string("/proc/stat") else {
            return Self::default();
        };
        let Some(line) = text.lines().next() else {
            return Self::default();
        };
        let mut it = line.split_whitespace();
        let _ = it.next(); // "cpu"
        let fields: Vec<u64> = it.filter_map(|s| s.parse().ok()).collect();
        if fields.len() < 4 {
            return Self::default();
        }
        // user nice system idle iowait irq softirq steal
        let idle = fields[3] + fields.get(4).copied().unwrap_or(0);
        let total: u64 = fields.iter().sum();
        CpuSample { total, idle }
    }

    /// 与上一次采样的差值，返回 0.0..1.0 的占用率。
    pub fn delta_usage(&self, prev: &CpuSample) -> f32 {
        let dt = self.total.saturating_sub(prev.total);
        let di = self.idle.saturating_sub(prev.idle);
        if dt == 0 {
            return 0.0;
        }
        let busy = dt.saturating_sub(di);
        (busy as f32 / dt as f32).clamp(0.0, 1.0)
    }
}

/// 一个进程的身份信息。
#[derive(Debug, Clone)]
pub struct ProcInfo {
    pub pid: i32,
    pub comm: String,
    pub cmdline: String,
}

/// 枚举所有进程，返回 (pid, comm, cmdline)。
///
/// 跳过内核线程（cmdline 为空）和自己。这个开销比想象中小：
/// 一般几百个进程，一次扫描几十毫秒。但我们只在名单非空时才扫描，
/// 且间隔拉长到秒级，不影响性能。
pub fn scan_processes() -> Vec<ProcInfo> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return out;
    };

    for e in entries.flatten() {
        let name = e.file_name();
        let Some(name) = name.to_str() else { continue };
        let Ok(pid) = name.parse::<i32>() else { continue };

        // comm：进程名，最多 15 字符
        let comm = fs::read_to_string(format!("/proc/{pid}/comm"))
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if comm.is_empty() {
            continue;
        }

        // cmdline：只取 argv[0]（程序路径本身），不要整个命令行。
        //
        // 为什么：拿完整 cmdline 去匹配会导致误报——shell 里随便敲一句
        // 含关键字的命令（比如 grep box64、systemctl status steam_app）
        // 都会命中名单，把系统误判成在跑游戏。实测踩过这个坑。
        //
        // argv[0] 是真正的可执行文件名，语义上更准确。
        let raw = fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
        if raw.is_empty() {
            continue; // 内核线程
        }
        let argv0 = raw.split(|&b| b == 0).next().unwrap_or(&[]);
        let cmdline = String::from_utf8_lossy(argv0).trim().to_string();
        if cmdline.is_empty() {
            continue;
        }

        out.push(ProcInfo { pid, comm, cmdline });
    }
    out
}

/// 负载等级，对应 uperf 的三个场景。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LoadLevel {
    Idle,
    Moderate,
    Heavy,
}

impl LoadLevel {
    /// 按总 CPU 占用判定。
    ///
    /// 阈值参照 uperf 的 heavyload_detector：超过 45% 视为重负载，
    /// 低于 12% 视为空闲。中间是常规。
    pub fn from_usage(usage: f32) -> Self {
        if usage >= 0.45 {
            LoadLevel::Heavy
        } else if usage <= 0.12 {
            LoadLevel::Idle
        } else {
            LoadLevel::Moderate
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_delta() {
        let prev = CpuSample { total: 1000, idle: 800 };
        let cur = CpuSample { total: 2000, idle: 1600 };
        // 1000 tick 中 200 忙 => 0.2
        assert!((cur.delta_usage(&prev) - 0.2).abs() < 0.01);
    }

    #[test]
    fn level_thresholds() {
        assert_eq!(LoadLevel::from_usage(0.05), LoadLevel::Idle);
        assert_eq!(LoadLevel::from_usage(0.30), LoadLevel::Moderate);
        assert_eq!(LoadLevel::from_usage(0.60), LoadLevel::Heavy);
    }
}
