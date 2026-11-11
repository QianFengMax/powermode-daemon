//! uperf-linux 联动层。
//!
//! ## 为什么要联动
//!
//! 这台机器上可能同时存在两个调频器：
//!   - 本守护进程（powermode-daemon）
//!   - uperf-linux（如果用户装了并开着）
//!
//! **两者不能同时生效**：实测 uperf 运行时会覆盖我们写入的
//! scaling_max_freq，导致档位切了但频率没变。
//!
//! 所以必须是「二选一」的互斥关系。这个模块负责：
//!   1. 检测 uperf 是否在运行
//!   2. 如果在运行，把档位同步给它（由它调频），我们不写 sysfs
//!   3. 如果不在运行，我们自己调频
//!
//! ## 为什么用 D-Bus 而不是文件
//!
//! uperf 没有状态文件协议，只有 D-Bus 的 SetMode。它的
//! `/run/uperf-linux/cur_powermode` 是只读的运行时状态，写它没用。

use std::process::Command;

const UPERF_NAME: &str = "org.uperflinux.Daemon";
const UPERF_PATH: &str = "/org/uperflinux/Daemon";
const UPERF_IFACE: &str = "org.uperflinux.Daemon";

/// uperf 的可用状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UperfState {
    /// 还没检测过
    Unknown,
    /// 在运行，可以接管调频
    Running,
    /// 没装或没运行
    Absent,
}

/// GNOME 档位 -> uperf 模式字符串。
///
/// **必须与 GNOME 的三档严格一一对应**：
///   power-saver  <->  powersave
///   balanced     <->  balance
///   performance  <->  performance
pub fn to_uperf_mode(gnome_mode: &str) -> Option<&'static str> {
    match gnome_mode {
        "powersave" | "power-saver" => Some("powersave"),
        "balance" | "balanced" => Some("balance"),
        "performance" => Some("performance"),
        "fast" => Some("performance"), // fast 没有对应档，用最高档
        _ => None,
    }
}

/// 检测 uperf 是否在运行。
///
/// 用 systemctl is-active 而不是 D-Bus 探测：后者在服务未启动时会
/// 触发 systemd 尝试激活（虽然会失败），多一次无谓的往返。
pub fn detect() -> UperfState {
    let Ok(out) = Command::new("systemctl")
        .args(["is-active", "uperf-linux.service"])
        .output()
    else {
        return UperfState::Absent;
    };
    if String::from_utf8_lossy(&out.stdout).trim() == "active" {
        UperfState::Running
    } else {
        UperfState::Absent
    }
}

/// 把档位同步给 uperf。
///
/// 只在 uperf 运行时调用。失败返回 Err，调用方应记录但不中断调度——
/// uperf 半路挂掉的话我们下一轮检测会切回自己调频。
pub fn set_mode(gnome_mode: &str) -> Result<(), String> {
    let Some(mode) = to_uperf_mode(gnome_mode) else {
        return Err(format!("无法映射档位: {gnome_mode}"));
    };

    let out = Command::new("busctl")
        .args([
            "--system", "call",
            UPERF_NAME, UPERF_PATH, UPERF_IFACE,
            "SetMode", "s", mode,
        ])
        .output()
        .map_err(|e| e.to_string())?;

    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(err.trim().to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gnome_to_uperf_mapping() {
        // 严格一一对应
        assert_eq!(to_uperf_mode("power-saver"), Some("powersave"));
        assert_eq!(to_uperf_mode("balanced"), Some("balance"));
        assert_eq!(to_uperf_mode("performance"), Some("performance"));

        // 内部简写形式也要认
        assert_eq!(to_uperf_mode("powersave"), Some("powersave"));
        assert_eq!(to_uperf_mode("balance"), Some("balance"));

        // 未知档位返回 None
        assert_eq!(to_uperf_mode("turbo"), None);
    }

    #[test]
    fn fast_maps_to_performance() {
        // fast 在 uperf 里没有对应档，用最高档兜底
        assert_eq!(to_uperf_mode("fast"), Some("performance"));
    }
}
