# powermode-daemon

GNOME 电源模式扩展的性能调度后端。替代 uperf-linux，用 Rust 重写。

## 为什么存在

这台机器（小米平板 6S Pro / SM8550）上，GNOME 的电源档位只是**状态标记**——
tlp-pd 提供 `org.freedesktop.UPower.PowerProfiles`，切换它不改变任何 CPU 参数。

真正控制频率的是 uperf-linux。这个守护进程就是它的替代品：
更小的体积、更少的依赖、和扩展直接集成。

## 架构

```
┌─────────────────────────────────┐
│ GNOME 扩展（powermode@local）    │
│  写三个状态文件到 /run/powermode │
└────────────┬────────────────────┘
             │ 单向文件协议
┌────────────▼────────────────────┐
│ powermode-daemon（本程序，root） │
│  inotify 监听 + 周期采样         │
│  写 sysfs 调频                   │
└─────────────────────────────────┘
```

### 状态文件协议

| 文件 | 写入方 | 含义 |
|---|---|---|
| `extension-enabled` | 扩展 enable/disable | 扩展是否启用 |
| `daemon-enabled` | 设置页按钮 | 用户是否点了启动 |
| `mode` | 扩展切档时 | 当前档位 |
| `fast-list.conf` | 设置页 | 应用名单（可选，优先于 /etc） |

**双门控**：两个 enabled 都为 1 才运行。任一为 0 就恢复 CPU 后退出；
如果是扩展关闭导致的，还会 `systemctl disable` 自己——扩展都关了，
开机再拉起来没意义。

### 零轮询

用 inotify 监听目录，`poll()` 加超时：

```rust
wait_readable(inot.as_raw_fd(), TICK_MS)?   // 线程真正睡眠
```

没有事件时线程阻塞在内核，不耗 CPU。1 秒超时用于周期采样
（温度、负载），这不是忙轮询。

## 模块

| 文件 | 对应 uperf | 作用 |
|---|---|---|
| `power_model.rs` | power_model.c | 功耗曲线与选频（二分搜索） |
| `perapp.rs` | perapp_config.c | fast 名单匹配 |
| `proc.rs` | heavyload_detector.c + game_scanner.c | 负载采样、进程扫描 |
| `thermal.rs` | thermal.c | 温度阈值与降级 |
| `scene.rs` | state_machine.c | 场景状态机（idle/touch/boost…） |
| `touch.rs` | input_monitor.c | evdev 触摸监听 |
| `tasks.rs` | task_scheduler.c | CPU 亲和性与簇识别 |
| `config.rs` | config_parser.c | 读 uperf 格式的 config.json |

## 调度逻辑

每一秒做一次：

```
1. 读 /proc/stat 算 CPU 占用
2. 读触摸设备，喂给场景状态机
3. 读温度，判断是否需要降级
4. 扫 fast 名单（每 3 秒一次）
5. 综合出目标档位：
     用户档位
       ↓ 被名单覆盖（有匹配进程就用名单的档位）
       ↓ 被场景加成（触摸时提一档）
       ↓ 被负载兜底（重负载至少 performance）
       ↓ 被温控封顶
     最终档位
6. 档位变了才写 sysfs（避免频繁写入干扰内核调频器）
```

## 频率怎么算

不是「硬件上限 × 固定比例」，而是复刻 uperf 的功耗模型：

```
目标性能 = demand × (1 + margin) × efficiency
二分搜索找满足它的最低频率
```

功耗曲线四段（低到高）：空闲区 / 低频爬升 / 正常 / 高性能。
低频段功耗下降比频率快得多，所以省电档能用一半频率换掉 70% 功耗。

## 崩溃保护

改频率前把**硬件上限**（`cpuinfo_max_freq`）存进 `snapshot.json`。
下次启动时如果发现快照还在，说明上次异常退出，先恢复再继续。

用硬件上限而不是 `scaling_max_freq` 作恢复目标，是因为后者可能
已被上次运行压低，读它会把低值当成原始值。

## 安装

```bash
./install.sh
```

会构建、安装二进制和服务，并在 `/etc/powermode/fast-list.conf` 写一份
默认名单（覆盖 Steam、兼容层、常见手游）。

卸载：

```bash
./install.sh uninstall
```

## 配置

可选文件 `/etc/powermode/config.json`，兼容 uperf 格式。
只读三段：

- `modules.cpu.powerModel` —— 各簇功耗曲线
- `modules.thermal` —— 温度阈值
- `modules.switcher.hintDuration` —— 场景持续时间

文件不存在时用内置默认值（就是本机 uperf 的标定值）。

## 与 uperf 的差异

| 项 | uperf | 本实现 |
|---|---|---|
| 语言 | C（10355 行） | Rust（约 1500 行） |
| 进程调度接管 | 全局（记录并恢复所有线程） | 不接管 |
| cgroup 分组 | 有 | 无 |
| D-Bus 接口 | 有 | 用文件协议 |
| 图形界面 | 有（GTK） | 无，由 GNOME 扩展提供 |
| 配置向导 | 有 | 无 |

**不接管进程调度的取舍**：全局接管需要 250ms 扫一次全系统线程并
维护状态表，收益主要是「后台不抢前台」——这件事 uclamp_max 就能做到，
且可按需配置，不必默认接管。

## 许可

MIT
