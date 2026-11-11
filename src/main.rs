//! powermode-daemon —— 性能调度守护进程。
//!
//! ## 双重门控
//!
//! 是否运行由两个条件共同决定：
//!   1. 扩展是否启用（/run/powermode/extension-enabled）
//!   2. 用户是否点了启动按钮（/run/powermode/daemon-enabled）
//!
//! 只有两个都是 1 才运行。任一为 0 就恢复 CPU 后退出。
//! 若是扩展关闭导致的，还要摘掉自己的开机自启——扩展都没开,
//! 说明用户不需要它。
//!
//! ## 事件驱动，不轮询
//!
//! 用 inotify 监听状态目录。文件变化时内核唤醒，空闲时线程阻塞在
//! read() 上，CPU 占用为零。之前的 2 秒 sleep 轮询已废弃。

mod power_model;
mod perapp;
mod proc;
mod thermal;
mod scene;
mod touch;
mod tasks;
mod config;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

use power_model::{mode_params, select_freq, PowerModelEntry};

const STATE_DIR: &str = "/run/powermode";
const F_EXT: &str = "/run/powermode/extension-enabled";
const F_DAEMON: &str = "/run/powermode/daemon-enabled";
const F_MODE: &str = "/run/powermode/mode";
const F_SNAP: &str = "/run/powermode/snapshot.json";
const F_PID: &str = "/run/powermode/pid";
const UNIT: &str = "powermode-daemon.service";
/// fast 名单文件，扩展可通过设置页编辑
/// fast 名单默认路径（持久，重启不丢）
const FAST_LIST_DEFAULT: &str = "/etc/powermode/fast-list.conf";
/// 扩展可以在运行时写的临时覆盖路径
const FAST_LIST_RUNTIME: &str = "/run/powermode/fast-list.conf";
/// 采样间隔。线程 sleep 在内核，不唤醒就不耗 CPU，不是忙轮询
const TICK_MS: u64 = 1000;

/// 各簇功耗模型，取自 uperf 对 sm8550 的标定，顺序对应 policy 序号。
fn models() -> Vec<PowerModelEntry> {
    vec![
        PowerModelEntry {
            efficiency: 180.0, nr: 3, cpumask: "efficiency".into(),
            typical_power: 0.8, typical_freq: 2016.0,
            sweet_freq: 1512.0, plain_freq: 1109.0, free_freq: 504.0,
        },
        PowerModelEntry {
            efficiency: 265.0, nr: 4, cpumask: "performance".into(),
            typical_power: 1.1, typical_freq: 2803.0,
            sweet_freq: 2102.0, plain_freq: 1542.0, free_freq: 701.0,
        },
        PowerModelEntry {
            efficiency: 350.0, nr: 1, cpumask: "prime".into(),
            typical_power: 1.4, typical_freq: 2957.0,
            sweet_freq: 2218.0, plain_freq: 1626.0, free_freq: 739.0,
        },
    ]
}

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_sig(_s: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

fn install_signals() {
    unsafe {
        libc::signal(libc::SIGTERM, on_sig as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_sig as libc::sighandler_t);
    }
}

fn flag(path: &str) -> bool {
    fs::read_to_string(path).map(|s| s.trim() == "1").unwrap_or(false)
}

// ---- CPU 快照 ----

#[derive(serde::Serialize, serde::Deserialize)]
struct Policy { name: String, max_freq: String, min_freq: String, governor: String }

#[derive(serde::Serialize, serde::Deserialize)]
struct Snapshot { policies: Vec<Policy> }

fn cfroot() -> PathBuf { PathBuf::from("/sys/devices/system/cpu/cpufreq") }

fn capture() -> std::io::Result<Snapshot> {
    let mut policies = Vec::new();
    let root = cfroot();
    if !root.exists() { return Ok(Snapshot { policies }); }
    for e in fs::read_dir(&root)?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.starts_with("policy") { continue; }
        let d = e.path();
        let rd = |f: &str| fs::read_to_string(d.join(f))
            .map(|s| s.trim().to_string()).unwrap_or_default();
        // 用硬件上限做恢复目标：上次异常退出压低过 scaling_max 的话，
        // 读 scaling_max 会把低值当原始值。
        let mut hw_max = rd("cpuinfo_max_freq");
        if hw_max.is_empty() { hw_max = rd("scaling_max_freq"); }
        let mut hw_min = rd("cpuinfo_min_freq");
        if hw_min.is_empty() { hw_min = rd("scaling_min_freq"); }
        policies.push(Policy { name, max_freq: hw_max, min_freq: hw_min,
            governor: rd("scaling_governor") });
    }
    Ok(Snapshot { policies })
}

fn save_snap(s: &Snapshot) {
    if let Ok(j) = serde_json::to_string_pretty(s) {
        if let Ok(mut f) = fs::File::create(F_SNAP) {
            let _ = f.write_all(j.as_bytes());
            let _ = f.sync_all();
        }
    }
}

fn load_snap() -> Option<Snapshot> {
    serde_json::from_str(&fs::read_to_string(F_SNAP).ok()?).ok()
}

fn restore(s: &Snapshot) {
    let root = cfroot();
    for p in &s.policies {
        let d = root.join(&p.name);
        if !p.governor.is_empty() {
            let _ = fs::write(d.join("scaling_governor"), &p.governor);
        }
        if !p.max_freq.is_empty() {
            let _ = fs::write(d.join("scaling_max_freq"), &p.max_freq);
        }
        if !p.min_freq.is_empty() {
            let _ = fs::write(d.join("scaling_min_freq"), &p.min_freq);
        }
    }
    println!("[daemon] 已恢复 CPU 原始状态");
}

// ---- 频率应用 ----

/// 用功耗模型反查目标频率后写入 scaling_max_freq。
///
/// 不是「硬件上限 × 固定比例」——那样在低频段会过度降频，
/// 因为功耗随频率是分段曲线而非线性。
fn apply_mode_with(mode: &str, m: &[power_model::PowerModelEntry]) {
    let p = mode_params(mode);
    let root = cfroot();

    let mut pols: Vec<_> = match fs::read_dir(&root) {
        Ok(e) => e.flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("policy"))
            .collect(),
        Err(_) => return,
    };
    pols.sort_by_key(|e| e.file_name());

    for (i, e) in pols.iter().enumerate() {
        let Some(pm) = m.get(i) else { continue };
        let d = e.path();
        let target_mhz = select_freq(pm, p.demand, p.margin);
        let target_hz = (target_mhz * 1000.0) as u64;
        let hw_max = fs::read_to_string(d.join("cpuinfo_max_freq")).ok()
            .and_then(|s| s.trim().parse::<u64>().ok()).unwrap_or(u64::MAX);
        let v = target_hz.min(hw_max);
        let est = power_model::est_power(pm, v as f32 / 1000.0);
        if fs::write(d.join("scaling_max_freq"), v.to_string()).is_ok() {
            println!("[daemon] {} -> {} MHz（估算 {:.2}W）",
                e.file_name().to_string_lossy(), v / 1000, est);
        }
    }
    println!("[daemon] 应用档位: {mode}");
}

/// 兼容旧调用：用内置模型。
fn apply_mode(mode: &str) {
    let m = models();
    apply_mode_with(mode, &m);
}

// ---- 退出 ----

fn bye(snap: Option<&Snapshot>, disable_self: bool) -> ! {
    if let Some(s) = snap {
        restore(s);
    } else if let Some(s) = load_snap() {
        restore(&s);
    }
    let _ = fs::remove_file(F_SNAP);
    let _ = fs::remove_file(F_PID);
    if disable_self {
        println!("[daemon] 扩展未启用，取消开机自启");
        let _ = Command::new("systemctl").args(["disable", UNIT]).status();
    }
    println!("[daemon] 已退出");
    std::process::exit(0);
}


/// 等 inotify fd 可读，最多 timeout 毫秒。
///
/// inotify 0.10 只提供无限阻塞的 read_events_blocking，没有超时版本。
/// 这里用 poll() 包装：返回 Ok(true) 表示有事件可读，Ok(false) 表示超时，
/// 两者都正常；Err 只在 fd 出错时出现。
fn wait_readable(fd: i32, timeout_ms: u64) -> std::io::Result<bool> {
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let rc = unsafe { libc::poll(&mut pfd, 1, timeout_ms as libc::c_int) };
    if rc < 0 {
        let e = std::io::Error::last_os_error();
        if e.kind() == std::io::ErrorKind::Interrupted {
            return Ok(false);
        }
        return Err(e);
    }
    Ok(rc > 0)
}

fn main() {
    if let Some(stale) = load_snap() {
        println!("[daemon] 发现残留快照，先恢复");
        restore(&stale);
        let _ = fs::remove_file(F_SNAP);
    }

    let _ = fs::create_dir_all(STATE_DIR);
    let _ = fs::write(F_PID, std::process::id().to_string());
    install_signals();

    // 启动即判门控
    if !flag(F_EXT) {
        bye(None, true);
    }
    if !flag(F_DAEMON) {
        println!("[daemon] 调度未启用");
        bye(None, false);
    }

    println!("[daemon] 启动");

    // 配置：优先读 /etc/powermode/config.json，不存在用内置默认值
    let cfg = config::Config::load();

    let base = match capture() {
        Ok(s) => s,
        Err(e) => { eprintln!("[daemon] 读 CPU 状态失败: {e}"); std::process::exit(1); }
    };
    save_snap(&base);

    // 用户指定的基础档位
    let mut user_mode = fs::read_to_string(F_MODE).ok()
        .map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
        .unwrap_or_else(|| "balance".to_string());

    // fast 名单
    // 名单：优先用 /run 下的（扩展写的），没有就用 /etc 下的持久版本
    let list_path = if std::path::Path::new(FAST_LIST_RUNTIME).exists() {
        FAST_LIST_RUNTIME
    } else {
        FAST_LIST_DEFAULT
    };
    let mut rules = perapp::PerAppRules::load(list_path);
    let mut last_rule_check = std::time::Instant::now();
    let mut rule_override: Option<String> = None;

    // 负载采样状态
    let mut prev_sample = proc::CpuSample::read();
    let mut thermal_state = thermal::ThermalState::Normal;
    let tcfg = cfg.thermal;

    let mut applied = String::new();
    apply_mode_with(&user_mode, &cfg.power_models);
    applied = user_mode.clone();

    // 场景状态机 + 触摸设备
    let mut scene_sm = scene::StateMachine::new();
    let touch_devs = touch::find_touchscreens();
    if !touch_devs.is_empty() {
        println!("[daemon] 触摸屏: {}",
            touch_devs.iter().map(|d| d.name.as_str()).collect::<Vec<_>>().join(", "));
    }

    // ---- inotify：空闲时阻塞，带超时用于周期采样 ----
    let mut inot = match inotify::Inotify::init() {
        Ok(i) => i,
        Err(e) => { eprintln!("[daemon] inotify 失败: {e}"); bye(Some(&base), false); }
    };

    use inotify::WatchMask;
    if let Err(e) = inot.watches().add(
        Path::new(STATE_DIR),
        WatchMask::MODIFY | WatchMask::CLOSE_WRITE,
    ) {
        eprintln!("[daemon] 监听失败: {e}");
        bye(Some(&base), false);
    }

    println!("[daemon] 监听 {STATE_DIR}（事件驱动 + {}ms 采样）", TICK_MS);

    let mut buf = [0u8; 4096];
    loop {
        if STOP.load(Ordering::SeqCst) {
            println!("[daemon] 收到停止信号");
            break;
        }

        // 带超时阻塞：没有事件时最多等 TICK_MS，用于周期采样
        // 这不是忙轮询——线程在 read() 上真正睡眠，内核不唤醒就不耗 CPU
        use std::os::unix::io::AsRawFd;
        let readable = match wait_readable(inot.as_raw_fd(), TICK_MS) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[daemon] poll 失败: {e}");
                break;
            }
        };
        let events = if readable {
            match inot.read_events_blocking(&mut buf) {
                Ok(e) => Some(e),
                Err(e) => {
                    if e.kind() == std::io::ErrorKind::Interrupted { continue; }
                    eprintln!("[daemon] 读事件失败: {e}");
                    break;
                }
            }
        } else {
            None
        };

        if let Some(events) = events {
            let (mut mode_changed, mut stop, mut kill_self) = (false, false, false);
            for ev in events {
                let Some(n) = ev.name.as_ref() else { continue };
                let n = n.to_string_lossy();
                if n == "mode" {
                    mode_changed = true;
                } else if n == "extension-enabled" || n == "daemon-enabled" {
                    if !flag(F_EXT) { stop = true; kill_self = true; }
                    else if !flag(F_DAEMON) { stop = true; }
                }
            }

            if stop {
                println!("[daemon] 门控不再满足");
                bye(Some(&base), kill_self);
            }

            if mode_changed {
                let want = fs::read_to_string(F_MODE).ok()
                    .map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
                if let Some(w) = want {
                    if w != user_mode {
                        user_mode = w;
                        println!("[daemon] 用户档位变更: {user_mode}");
                    }
                }
            }
        }

        // ---- 周期采样：温控 + 负载 + 名单 ----
        let cur_sample = proc::CpuSample::read();
        let usage = cur_sample.delta_usage(&prev_sample);
        prev_sample = cur_sample;

        let load = proc::LoadLevel::from_usage(usage);

        // 触摸事件：非阻塞读，有事件就喂给状态机
        read_touch_events(&touch_devs, &mut scene_sm);
        scene_sm.tick();
        scene_sm.set_heavy_load(load == proc::LoadLevel::Heavy);

        // 温控
        if let Some(t) = thermal::max_temp() {
            let ns = thermal::ThermalState::evaluate(&tcfg, t, thermal_state);
            if ns != thermal_state {
                println!("[daemon] 温控 {} -> {}（{} mC）",
                    thermal_state.as_str(), ns.as_str(), t);
                thermal_state = ns;
            }
        }

        // fast 名单：每隔几秒扫一次进程，结果缓存到 rule_override。
        // 注意必须缓存结果而不是缓存"是否该扫描"——否则两次扫描之间
        // 会被上次的匹配结果一直粘住，进程退了也降不下来。
        if !rules.is_empty() && last_rule_check.elapsed().as_secs() >= 3 {
            last_rule_check = std::time::Instant::now();
            let mut found: Option<String> = None;
            for p in proc::scan_processes() {
                if let Some(m) = rules.lookup(&p.comm, &p.cmdline) {
                    found = Some(m.as_str().to_string());
                    break;
                }
            }
            if found != rule_override {
                if let Some(ref f) = found {
                    println!("[daemon] 名单命中: {f}");
                } else if rule_override.is_some() {
                    println!("[daemon] 名单进程已退出");
                }
                rule_override = found;
            }
        }
        let mut effective = rule_override.clone().unwrap_or_else(|| user_mode.clone());

        // 负载兜底：重负载但档位偏低时抬一档，空闲时允许降档
        let eff_rank = mode_rank(&effective);
        let effective = match load {
            proc::LoadLevel::Heavy if eff_rank < 2 => "performance".to_string(),
            _ => effective,
        };

        // 场景加成：触摸/切换等场景临时提一档，改善流畅度
        let bonus = scene_sm.demand_bonus();
        let effective = if bonus > 0.0 && mode_rank(&effective) < 2 {
            bump_by_bonus(&effective, bonus)
        } else {
            effective
        };

        // 温控封顶
        let capped = cap_by_thermal(&effective, thermal_state.mode_cap());

        // 只在真正变化时写 sysfs。每秒无脑写会造成：
        // 1) 日志刷屏 2) 频繁 sysfs 写入干扰内核调频器
        if capped != applied {
            if capped != user_mode {
                println!("[daemon] 自动调度: {} -> {}（负载 {:?}）",
                    applied, capped, load);
            }
            apply_mode_with(&capped, &cfg.power_models);
            applied = capped;
        }
    }

    bye(Some(&base), false);
}


/// 打开所有触摸设备（非阻塞），逐个读一遍待处理事件喂给状态机。
///
/// 每次都重新 open 是有意的：触摸设备少（通常 1-2 个），open 的开销
/// 远小于维护一个 fd 池的复杂度。而且这样能自动适配设备热插拔。
fn read_touch_events(devs: &[touch::TouchDevice], sm: &mut scene::StateMachine) {
    use std::io::Read;

    for d in devs {
        let Ok(mut f) = std::fs::OpenOptions::new().read(true).open(&d.path) else {
            continue;
        };
        // 非阻塞：没事件就立刻返回，不拖慢采样循环
        let flags = unsafe { libc::fcntl(std::os::unix::io::AsRawFd::as_raw_fd(&f), libc::F_GETFL) };
        unsafe {
            libc::fcntl(
                std::os::unix::io::AsRawFd::as_raw_fd(&f),
                libc::F_SETFL,
                flags | libc::O_NONBLOCK,
            );
        }

        let mut buf = [0u8; 24];
        // 最多读 64 个事件，防止积压时卡太久
        for _ in 0..64 {
            match f.read(&mut buf) {
                Ok(24) => {
                    if let Some((ty, code, val)) = touch::parse_event(&buf) {
                        match touch::classify(ty, code, val) {
                            Some(touch::TouchEvent::Down) => sm.on_touch(),
                            Some(touch::TouchEvent::Up) => {}
                            None => {}
                        }
                    }
                }
                _ => break,
            }
        }
    }
}

/// 按场景加成提升档位。
///
/// 只提升一档，不做跨档跳跃：触摸时从 powersave 直接跳到 performance
/// 会让功耗曲线突变，反而影响续航。
fn bump_by_bonus(mode: &str, bonus: f32) -> String {
    let rank = mode_rank(mode);
    // bonus >= 0.15 才值得提一档，太小没感知
    if bonus < 0.15 {
        return mode.to_string();
    }
    match rank {
        0 => "balance".to_string(),
        1 => "performance".to_string(),
        _ => mode.to_string(),
    }
}

/// 档位排序，用于比较高低。
fn mode_rank(m: &str) -> u8 {
    match m {
        "powersave" => 0,
        "balance" => 1,
        "performance" => 2,
        "fast" => 3,
        _ => 1,
    }
}

/// 按温控上限裁剪档位。
fn cap_by_thermal(mode: &str, cap: u8) -> String {
    if mode_rank(mode) <= cap {
        return mode.to_string();
    }
    match cap {
        0 => "powersave".to_string(),
        1 => "balance".to_string(),
        2 => "performance".to_string(),
        _ => mode.to_string(),
    }
}