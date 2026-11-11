//! 配置解析。
//!
//! 兼容 uperf 的 config.json 格式，但**只读取我们真正用得到的字段**。
//!
//! 为什么不全部读：uperf 的配置有 20+ 段（input/thermal/sched/affinity/prio...），
//! 其中大部分是给 C 版实现用的内部参数。我们只用得到：
//!
//!   cpu.powerModel  各簇的功耗曲线（决定选频）
//!   thermal         温度阈值
//!   switcher        hintDuration（场景持续时间）
//!
//! 文件路径默认 /etc/powermode/config.json，不存在时用内置默认值。
//! 这样既能让用户复用现有 uperf 配置，也能在没配置时直接跑。

use std::fs;

use crate::power_model::PowerModelEntry;
use crate::thermal::ThermalConfig;

const DEFAULT_PATH: &str = "/etc/powermode/config.json";

/// 精简后的配置。
#[derive(Debug, Clone)]
pub struct Config {
    pub power_models: Vec<PowerModelEntry>,
    pub thermal: ThermalConfig,
    /// 场景持续时间（毫秒），从 hintDuration 换算
    pub hint_touch_ms: u64,
    pub hint_trigger_ms: u64,
    pub hint_gesture_ms: u64,
    pub hint_switch_ms: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            power_models: crate::models(),
            thermal: ThermalConfig::default(),
            hint_touch_ms: 4000,
            hint_trigger_ms: 30,
            hint_gesture_ms: 100,
            hint_switch_ms: 400,
        }
    }
}

impl Config {
    /// 从文件加载；任何环节失败都退回默认值，不让守护进程起不来。
    pub fn load() -> Self {
        let path = std::env::var("POWERMODE_CONFIG")
            .unwrap_or_else(|_| DEFAULT_PATH.to_string());
        let Ok(text) = fs::read_to_string(&path) else {
            return Self::default();
        };
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
            println!("[daemon] 配置解析失败，使用默认值: {path}");
            return Self::default();
        };

        let mut cfg = Self::default();

        // 功耗模型
        if let Some(models) = json.pointer("/modules/cpu/powerModel")
            .and_then(|v| v.as_array())
        {
            let mut parsed = Vec::new();
            for m in models {
                let Some(eff) = m.get("efficiency").and_then(|v| v.as_f64()) else {
                    continue;
                };
                parsed.push(PowerModelEntry {
                    efficiency: eff as f32,
                    nr: m.get("nr").and_then(|v| v.as_u64()).unwrap_or(1) as u32,
                    cpumask: m.get("cpumask").and_then(|v| v.as_str())
                        .unwrap_or("all").to_string(),
                    typical_power: m.get("typicalPower")
                        .and_then(|v| v.as_f64()).unwrap_or(1.0) as f32,
                    typical_freq: m.get("typicalFreq")
                        .and_then(|v| v.as_f64()).unwrap_or(2000.0) as f32,
                    sweet_freq: m.get("sweetFreq")
                        .and_then(|v| v.as_f64()).unwrap_or(1800.0) as f32,
                    plain_freq: m.get("plainFreq")
                        .and_then(|v| v.as_f64()).unwrap_or(1200.0) as f32,
                    free_freq: m.get("freeFreq")
                        .and_then(|v| v.as_f64()).unwrap_or(600.0) as f32,
                });
            }
            if !parsed.is_empty() {
                cfg.power_models = parsed;
            }
        }

        // 温控
        if let Some(t) = json.pointer("/modules/thermal") {
            let get = |k: &str, d: i32| {
                t.get(k).and_then(|v| v.as_i64()).unwrap_or(d as i64) as i32
            };
            cfg.thermal = ThermalConfig {
                warn: get("warn_temp", 85000),
                throttle: get("throttle_temp", 95000),
                critical: get("critical_temp", 110000),
                recovery: get("recovery_temp", 88000),
            };
        }

        // hintDuration（秒 -> 毫秒）
        if let Some(h) = json.pointer("/modules/switcher/hintDuration") {
            let ms = |k: &str, d: u64| {
                (h.get(k).and_then(|v| v.as_f64()).unwrap_or(d as f64 / 1000.0)
                    * 1000.0) as u64
            };
            cfg.hint_touch_ms = ms("touch", 4000);
            cfg.hint_trigger_ms = ms("trigger", 30);
            cfg.hint_gesture_ms = ms("gesture", 100);
            cfg.hint_switch_ms = ms("switch", 400);
        }

        println!("[daemon] 配置已加载: {path}（{} 个功耗模型）",
            cfg.power_models.len());
        cfg
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let c = Config::default();
        assert!(!c.power_models.is_empty());
        assert!(c.thermal.warn < c.thermal.throttle);
        assert!(c.thermal.throttle < c.thermal.critical);
        assert!(c.hint_touch_ms > c.hint_trigger_ms);
    }

    #[test]
    fn missing_file_falls_back() {
        std::env::set_var("POWERMODE_CONFIG", "/nonexistent/x.json");
        let c = Config::load();
        assert!(!c.power_models.is_empty());
        std::env::remove_var("POWERMODE_CONFIG");
    }
}
