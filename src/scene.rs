//! 场景状态机。
//!
//! 复刻 uperf 的 state_machine.c。核心思想：
//!
//! 档位（powersave/balance/performance）只是「基线」，
//! 实际调度参数由「当前场景」在这个基线上做微调。
//!
//! 场景：
//!   Idle     长时间无操作  -> 更激进降频
//!   Touch    手指按屏幕    -> 临时提频保流畅
//!   Trigger  短促动作      -> 瞬时提频
//!   Gesture  滑动/系统手势 -> 中等提频
//!   Switch   应用切换      -> 短暂提频
//!   Boost    重负载        -> 最高频
//!
//! 每个场景有超时，到期自动回到 Idle。这样即使触摸事件丢了也不会卡在提频态。

use std::time::{Duration, Instant};

/// 场景。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scene {
    Idle,
    Touch,
    Trigger,
    Gesture,
    Switch,
    Boost,
}

impl Scene {
    pub fn as_str(&self) -> &'static str {
        match self {
            Scene::Idle => "idle",
            Scene::Touch => "touch",
            Scene::Trigger => "trigger",
            Scene::Gesture => "gesture",
            Scene::Switch => "switch",
            Scene::Boost => "boost",
        }
    }

    /// 该场景的持续时间。到期自动回 Idle。
    ///
    /// 数值取自 uperf config.json 的 hintDuration：
    ///   touch 4.0s（手指可能一直按着，给长一点）
    ///   trigger 0.03s（极短，只是一次点击）
    ///   gesture 0.1s
    ///   switch 0.4s（切应用时的过渡）
    pub fn duration_ms(&self) -> u64 {
        match self {
            Scene::Idle => 0,
            Scene::Touch => 4000,
            Scene::Trigger => 30,
            Scene::Gesture => 100,
            Scene::Switch => 400,
            Scene::Boost => 0, // 由负载决定，不超时
        }
    }

    /// 该场景对频率的需求加成。0.0 = 无加成，1.0 = 满频。
    pub fn demand_bonus(&self) -> f32 {
        match self {
            Scene::Idle => 0.0,
            Scene::Touch => 0.15,
            Scene::Trigger => 0.20,
            Scene::Gesture => 0.10,
            Scene::Switch => 0.12,
            Scene::Boost => 0.40,
        }
    }
}

/// 状态机。
pub struct StateMachine {
    scene: Scene,
    since: Instant,
    /// 这次 Boost 是否由重负载触发（由负载决定何时退出）
    boost_by_load: bool,
}

impl StateMachine {
    pub fn new() -> Self {
        Self { scene: Scene::Idle, since: Instant::now(), boost_by_load: false }
    }

    pub fn scene(&self) -> Scene {
        self.scene
    }

    /// 切到某场景并重置计时。
    fn enter(&mut self, s: Scene) {
        if self.scene != s {
            self.scene = s;
            self.since = Instant::now();
        }
    }

    /// 触摸按下或移动。
    pub fn on_touch(&mut self) {
        // Boost 优先级最高，不被触摸打断
        if self.scene == Scene::Boost {
            return;
        }
        self.enter(Scene::Touch);
    }

    /// 一次短促点击。
    pub fn on_trigger(&mut self) {
        if self.scene == Scene::Boost {
            return;
        }
        self.enter(Scene::Trigger);
    }

    /// 滑动或系统手势。
    pub fn on_gesture(&mut self) {
        if self.scene == Scene::Boost {
            return;
        }
        self.enter(Scene::Gesture);
    }

    /// 应用切换。
    pub fn on_window_switch(&mut self) {
        if self.scene == Scene::Boost {
            return;
        }
        self.enter(Scene::Switch);
    }

    /// 重负载开始/结束。这是唯一由负载驱动、不靠超时的场景。
    pub fn set_heavy_load(&mut self, heavy: bool) {
        if heavy {
            if self.scene != Scene::Boost {
                self.boost_by_load = true;
                self.enter(Scene::Boost);
            }
        } else if self.scene == Scene::Boost && self.boost_by_load {
            self.boost_by_load = false;
            self.enter(Scene::Idle);
        }
    }

    /// 每 tick 调用一次：超时的场景自动回 Idle。
    pub fn tick(&mut self) {
        if self.scene == Scene::Idle || self.scene == Scene::Boost {
            return; // 这两个不由超时驱动
        }
        let limit = Duration::from_millis(self.scene.duration_ms());
        if self.since.elapsed() >= limit {
            self.enter(Scene::Idle);
        }
    }

    /// 当前场景对档位需求的加成。
    pub fn demand_bonus(&self) -> f32 {
        self.scene.demand_bonus()
    }
}

impl Default for StateMachine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touch_then_timeout() {
        let mut sm = StateMachine::new();
        assert_eq!(sm.scene(), Scene::Idle);
        sm.on_touch();
        assert_eq!(sm.scene(), Scene::Touch);
        // 手动把计时器拨回去模拟超时
        sm.since = Instant::now() - Duration::from_millis(5000);
        sm.tick();
        assert_eq!(sm.scene(), Scene::Idle);
    }

    #[test]
    fn boost_survives_touch() {
        let mut sm = StateMachine::new();
        sm.set_heavy_load(true);
        assert_eq!(sm.scene(), Scene::Boost);
        sm.on_touch();
        assert_eq!(sm.scene(), Scene::Boost, "Boost 不应被触摸打断");
        sm.set_heavy_load(false);
        assert_eq!(sm.scene(), Scene::Idle);
    }

    #[test]
    fn bonus_ordering() {
        assert!(Scene::Idle.demand_bonus() < Scene::Touch.demand_bonus());
        assert!(Scene::Touch.demand_bonus() < Scene::Boost.demand_bonus());
    }
}
