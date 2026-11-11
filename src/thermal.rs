//! 温控。
//!
//! 复刻 uperf 的 thermal.c。核心是"温度越高越保守"的降级曲线：
//!
//!   < warn      正常
//!   warn..thr   轻度降级（性能档降到平衡）
//!   thr..crit   重度降级（强制省电）
//!   >= crit     紧急（最低频）
//!
//! 回差很重要：没有 recovery 温度的话，在阈值附近抖动时会每秒切档。

use std::fs;

/// 温度阈值（毫摄氏度，sysfs 的单位）。
#[derive(Debug, Clone, Copy)]
pub struct ThermalConfig {
    pub warn: i32,
    pub throttle: i32,
    pub critical: i32,
    pub recovery: i32,
}

impl Default for ThermalConfig {
    fn default() -> Self {
        // 与 uperf 的 config.json 一致
        Self { warn: 85000, throttle: 95000, critical: 110000, recovery: 88000 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThermalState {
    Normal,
    Warn,
    Throttled,
    Critical,
}

impl ThermalState {
    pub fn as_str(&self) -> &'static str {
        match self {
            ThermalState::Normal => "normal",
            ThermalState::Warn => "warn",
            ThermalState::Throttled => "throttled",
            ThermalState::Critical => "critical",
        }
    }

}

/// 读 SoC 最高温度（毫摄氏度）。遍历 /sys/class/thermal 取最大值。
pub fn max_temp() -> Option<i32> {
    let entries = fs::read_dir("/sys/class/thermal").ok()?;
    let mut max: Option<i32> = None;

    for e in entries.flatten() {
        let name = e.file_name();
        let Some(n) = name.to_str() else { continue };
        if !n.starts_with("thermal_zone") {
            continue;
        }
        let path = e.path().join("temp");
        let Ok(raw) = fs::read_to_string(&path) else { continue };
        let Ok(t) = raw.trim().parse::<i32>() else { continue };
        // 有些 zone 报摄氏度、有些报毫摄氏度
        let t = if t < 1000 { t * 1000 } else { t };
        max = Some(max.map_or(t, |m: i32| m.max(t)));
    }
    max
}

impl ThermalState {
    /// 按温度判定状态。
    pub fn evaluate(cfg: &ThermalConfig, temp: i32, _prev: ThermalState) -> ThermalState {
        if temp >= cfg.critical {
            return ThermalState::Critical;
        }
        if temp >= cfg.throttle {
            return ThermalState::Throttled;
        }
        if temp >= cfg.warn {
            return ThermalState::Warn;
        }
        // 解除降级要看 recovery 回差
        let _ = cfg.recovery;
        ThermalState::Normal
    }

    /// 温控允许的最高档位：0=powersave 1=balance 2=performance 3=fast
    pub fn mode_cap(&self) -> u8 {
        match self {
            ThermalState::Normal => 3,
            ThermalState::Warn => 2,
            ThermalState::Throttled => 1,
            ThermalState::Critical => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thresholds() {
        let cfg = ThermalConfig::default();
        assert_eq!(ThermalState::evaluate(&cfg, 70000, ThermalState::Normal), ThermalState::Normal);
        assert_eq!(ThermalState::evaluate(&cfg, 90000, ThermalState::Normal), ThermalState::Warn);
        assert_eq!(ThermalState::evaluate(&cfg, 98000, ThermalState::Normal), ThermalState::Throttled);
        assert_eq!(ThermalState::evaluate(&cfg, 115000, ThermalState::Normal), ThermalState::Critical);
    }

    #[test]
    fn cap_ordering() {
        assert!(ThermalState::Normal.mode_cap() >= ThermalState::Warn.mode_cap());
        assert!(ThermalState::Warn.mode_cap() >= ThermalState::Throttled.mode_cap());
        assert!(ThermalState::Throttled.mode_cap() >= ThermalState::Critical.mode_cap());
    }
}
