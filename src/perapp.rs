//! fast 名单（per-app 规则）。
//!
//! 复刻 uperf 的 perapp_config.c。名单文件是纯文本，每行一条规则：
//!
//!   <进程名模式> <模式>
//!
//! 支持冒号或空格分隔，# 开头为注释。模式是子串匹配（不区分大小写），
//! 对照 /proc/[pid]/comm 和 cmdline。
//!
//! 顺序敏感：第一个匹配的规则生效，所以具体规则要排在宽泛规则前面。
//! 例如 "steam_app_730" 包含 "steam"，steam 规则必须放后面。

use std::fs;
use std::path::Path;

/// 条目上限，与 uperf 的 PERAPP_MAX_RULES 一致。
const MAX_RULES: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Powersave,
    Balance,
    Performance,
    /// fast = 比 performance 更激进（不限频），用于重负载应用
    Fast,
}

impl Mode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Mode::Powersave => "powersave",
            Mode::Balance => "balance",
            Mode::Performance => "performance",
            Mode::Fast => "fast",
        }
    }

    pub fn parse(s: &str) -> Mode {
        match s.trim().to_ascii_lowercase().as_str() {
            "powersave" => Mode::Powersave,
            "performance" => Mode::Performance,
            "fast" => Mode::Fast,
            _ => Mode::Balance,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Rule {
    pub pattern: String,
    pub mode: Mode,
}

#[derive(Debug, Default)]
pub struct PerAppRules {
    pub rules: Vec<Rule>,
}

impl PerAppRules {
    /// 从文件加载。文件不存在不算错误，返回空规则表。
    pub fn load(path: &str) -> Self {
        let mut rules = Vec::new();
        let Ok(text) = fs::read_to_string(Path::new(path)) else {
            return PerAppRules { rules };
        };

        for line in text.lines() {
            if rules.len() >= MAX_RULES {
                break;
            }
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            let (pat, mode_str) = split_rule(line);
            let pat = pat.trim();
            let mode_str = mode_str.trim();
            if pat.is_empty() || mode_str.is_empty() {
                continue;
            }

            rules.push(Rule {
                pattern: pat.to_string(),
                mode: Mode::parse(mode_str),
            });
        }

        println!("[daemon] fast 名单: 加载 {} 条规则", rules.len());
        PerAppRules { rules }
    }

    /// 按进程名和 cmdline 查找匹配的模式。第一个匹配的规则生效。
    pub fn lookup(&self, comm: &str, cmdline: &str) -> Option<Mode> {
        for r in &self.rules {
            if pattern_matches(&r.pattern, comm) || pattern_matches(&r.pattern, cmdline) {
                return Some(r.mode);
            }
        }
        None
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }
}

/// 支持 "pattern:mode" 和 "pattern mode" 两种写法。
fn split_rule(line: &str) -> (&str, &str) {
    let space = line.find(' ');
    let colon = line.find(':');
    match (space, colon) {
        (Some(s), Some(c)) if c < s => (&line[..c], &line[c + 1..]),
        (Some(s), _) => (&line[..s], &line[s + 1..]),
        (None, Some(c)) => (&line[..c], &line[c + 1..]),
        (None, None) => (line, ""),
    }
}

/// 子串匹配，不区分大小写。星号和点星匹配一切。
fn pattern_matches(pattern: &str, text: &str) -> bool {
    if pattern.is_empty() || text.is_empty() {
        return false;
    }
    if pattern == "*" || pattern == ".*" {
        return true;
    }
    text.to_ascii_lowercase()
        .contains(&pattern.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substring_match() {
        assert!(pattern_matches("steam_app_", "steam_app_730"));
        assert!(pattern_matches("STEAM", "steam_app_730"));
        assert!(!pattern_matches("firefox", "chromium"));
        assert!(pattern_matches("*", "anything"));
    }

    #[test]
    fn mode_parse() {
        assert_eq!(Mode::parse("performance"), Mode::Performance);
        assert_eq!(Mode::parse("FAST"), Mode::Fast);
        assert_eq!(Mode::parse("garbage"), Mode::Balance);
    }

    #[test]
    fn colon_and_space_split() {
        assert_eq!(split_rule("wine performance"), ("wine", "performance"));
        assert_eq!(split_rule("wine:performance"), ("wine", "performance"));
    }
}
