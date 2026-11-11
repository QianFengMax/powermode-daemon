//! 触摸输入监听。
//!
//! 复刻 uperf 的 input_monitor.c。直接读 evdev，只关心触摸屏的按下/抬起：
//!
//!   EV_KEY + BTN_TOUCH(0x14a)  手指按下/抬起
//!   EV_ABS + ABS_MT_POSITION_X/Y  坐标（用于判断滑动）
//!
//! 为什么要自己做：GNOME 的 Clutter 事件在合成器里，系统级守护进程读不到。
//! evdev 是唯一可靠来源，且不干扰正常输入（只读不 grab）。

use std::fs;
use std::path::PathBuf;

const EV_KEY: u16 = 0x01;
const EV_ABS: u16 = 0x03;
const BTN_TOUCH: u16 = 0x14a;
const ABS_MT_POSITION_X: u16 = 0x35;
const ABS_MT_POSITION_Y: u16 = 0x36;

/// 一个触摸屏设备。
pub struct TouchDevice {
    pub path: PathBuf,
    pub name: String,
}

/// 在 /dev/input 里找带 BTN_TOUCH 的设备。
///
/// 用 sysfs 的能力位图判断，不用 ioctl——后者需要额外的 EVIOCGBIT
/// 调用，而读 /sys/class/input/eventN/device/capabilities/key 更直接。
pub fn find_touchscreens() -> Vec<TouchDevice> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir("/sys/class/input") else {
        return out;
    };

    for e in entries.flatten() {
        let name = e.file_name();
        let Some(n) = name.to_str() else { continue };
        if !n.starts_with("event") {
            continue;
        }

        let base = e.path().join("device");
        let caps = fs::read_to_string(base.join("capabilities/key"))
            .unwrap_or_default();
        if !has_bit(&caps, BTN_TOUCH as u32 as usize) {
            continue;
        }

        let dev_name = fs::read_to_string(base.join("name"))
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| n.to_string());

        out.push(TouchDevice {
            path: PathBuf::from(format!("/dev/input/{n}")),
            name: dev_name,
        });
    }
    out
}

/// 解析 sysfs 十六进制能力位图，看某位是否为 1。
///
/// 格式是一串空格分隔的 64 位十六进制数，**低位字在后**，
/// 所以 bit 0 在最后一个字的最低位。
fn has_bit(mask: &str, bit: usize) -> bool {
    if mask.trim().is_empty() {
        return false;
    }
    let words: Vec<&str> = mask.split_whitespace().collect();
    let total_bits = words.len() * 64;
    if bit >= total_bits {
        return false;
    }
    let word_index = words.len() - 1 - (bit / 64);
    let Some(word) = words.get(word_index) else {
        return false;
    };
    let Ok(v) = u64::from_str_radix(word, 16) else {
        return false;
    };
    (v >> (bit % 64)) & 1 == 1
}

/// 从 24 字节的 input_event 里解析。返回 (type, code, value)。
///
/// 结构（小端）：timeval(16) + type(2) + code(2) + value(4)
pub fn parse_event(buf: &[u8]) -> Option<(u16, u16, i32)> {
    if buf.len() < 24 {
        return None;
    }
    let ty = u16::from_le_bytes([buf[16], buf[17]]);
    let code = u16::from_le_bytes([buf[18], buf[19]]);
    let value = i32::from_le_bytes([buf[20], buf[21], buf[22], buf[23]]);
    Some((ty, code, value))
}

/// 判定事件类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TouchEvent {
    Down,
    Up,
}

pub fn classify(ty: u16, code: u16, value: i32) -> Option<TouchEvent> {
    if ty == EV_KEY && code == BTN_TOUCH {
        return if value != 0 {
            Some(TouchEvent::Down)
        } else {
            Some(TouchEvent::Up)
        };
    }
    let _ = (ty, code, EV_ABS, ABS_MT_POSITION_X, ABS_MT_POSITION_Y);
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bit_parsing() {
        // 单字 "400" = 0x400，bit 10 置位
        assert!(has_bit("400", 10));
        assert!(!has_bit("400", 11));
        // 空位图
        assert!(!has_bit("", 0));
        // 越界
        assert!(!has_bit("1", 100));
    }

    #[test]
    fn event_parsing() {
        let mut buf = vec![0u8; 24];
        buf[16..18].copy_from_slice(&EV_KEY.to_le_bytes());
        buf[18..20].copy_from_slice(&BTN_TOUCH.to_le_bytes());
        buf[20..24].copy_from_slice(&1i32.to_le_bytes());
        let (ty, code, val) = parse_event(&buf).unwrap();
        assert_eq!(ty, EV_KEY);
        assert_eq!(code, BTN_TOUCH);
        assert_eq!(val, 1);
        assert_eq!(classify(ty, code, val), Some(TouchEvent::Down));
    }
}
