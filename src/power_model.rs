//! 功耗模型 —— 与 uperf 的 power_model.c 保持一致。
//!
//! 用分段线性插值估算某个频率下这个簇的功耗。四个区间（频率单位 MHz）：
//!
//!   [0 .. free]           空闲区，漏电主导，功耗约典型值的 30%
//!   [free .. plain]       低频爬升区，30% -> 50%
//!   [plain .. sweet]      正常区，50% -> 70%
//!   [sweet .. typical]    高性能区，70% -> 100%
//!
//! 曲线形状来自 uperf 的实测标定，不能改成线性——低频段功耗下降比频率
//! 下降快得多，这正是省电档能用一半频率换掉 70% 功耗的原因。

/// 单个 CPU 簇的功耗模型参数。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PowerModelEntry {
    pub efficiency: f32,
    pub nr: u32,
    pub cpumask: String,
    pub typical_power: f32,
    pub typical_freq: f32,
    pub sweet_freq: f32,
    pub plain_freq: f32,
    pub free_freq: f32,
}

/// 估算某频率下的功耗（瓦）。
pub fn est_power(pm: &PowerModelEntry, freq_mhz: f32) -> f32 {
    if freq_mhz <= 0.0 {
        return 0.0;
    }
    let f = freq_mhz.min(pm.typical_freq);
    let norm = f / pm.typical_freq;

    // 起点用平方模型标定在 typical_freq 处
    let mut power = pm.typical_power * norm * norm;

    if f <= pm.free_freq {
        let free_power = pm.typical_power * 0.3;
        power = free_power * (f / pm.free_freq);
    }

    if f > pm.free_freq && f <= pm.plain_freq {
        let t = (f - pm.free_freq) / (pm.plain_freq - pm.free_freq);
        let free_p = pm.typical_power * 0.3;
        let plain_p = pm.typical_power * 0.5;
        power = free_p + t * (plain_p - free_p);
    }

    if f > pm.plain_freq && f <= pm.sweet_freq {
        let t = (f - pm.plain_freq) / (pm.sweet_freq - pm.plain_freq);
        let plain_p = pm.typical_power * 0.5;
        let sweet_p = pm.typical_power * 0.7;
        power = plain_p + t * (sweet_p - plain_p);
    }

    if f > pm.sweet_freq {
        let t = (f - pm.sweet_freq) / (pm.typical_freq - pm.sweet_freq);
        let sweet_p = pm.typical_power * 0.7;
        power = sweet_p + t * (pm.typical_power - sweet_p);
    }

    power
}

/// 某频率下的相对性能（0..efficiency）。
pub fn perf_at_freq(pm: &PowerModelEntry, freq_mhz: f32) -> f32 {
    if freq_mhz <= 0.0 {
        return 0.0;
    }
    pm.efficiency * (freq_mhz / pm.typical_freq)
}

/// 反查：给定需求（0..1）和余量，找最低的能满足它的频率。
///
/// uperf 原实现是二分搜索，这里保持一致——直接按比例算会忽略功耗曲线
/// 在低频段的非线性，选出来的频率偏低。
pub fn select_freq(pm: &PowerModelEntry, demand: f32, margin: f32) -> f32 {
    let target_perf = demand * (1.0 + margin) * pm.efficiency;

    let mut lo = pm.free_freq;
    let mut hi = pm.typical_freq;

    for _ in 0..50 {
        let mid = (lo + hi) / 2.0;
        let perf = perf_at_freq(pm, mid);
        if perf >= target_perf {
            hi = mid;
        } else {
            lo = mid;
        }
        if hi - lo < 1.0 {
            break;
        }
    }
    hi
}

/// 三个档位的调度参数。
///
/// `demand` 是目标性能占满频的比例，`margin` 是额外余量。
/// 数值参照 uperf 的档位配置（slowLimitPower / margin）。
pub struct ModeParams {
    pub demand: f32,
    pub margin: f32,
}

pub fn mode_params(mode: &str) -> ModeParams {
    match mode {
        "powersave" => ModeParams { demand: 0.35, margin: 0.10 },
        "balanced" | "balance" => ModeParams { demand: 0.60, margin: 0.25 },
        "performance" => ModeParams { demand: 1.00, margin: 0.40 },
        _ => ModeParams { demand: 0.60, margin: 0.25 },
    }
}
