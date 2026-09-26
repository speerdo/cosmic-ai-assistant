//! Combining trigger edges across keyboards. Pure, so it is tested without
//! hardware.

use std::collections::BTreeSet;

use crate::Edge;

/// Which devices currently hold the trigger. The combined trigger is held
/// while any of them does: pressing it on the laptop while the Launch is
/// also held is one utterance, not two.
#[derive(Debug, Default)]
pub struct TriggerState {
    held: BTreeSet<u64>,
}

impl TriggerState {
    /// Feed one `EV_KEY` value (0 release, 1 press, 2 autorepeat) from
    /// device `id`. Returns the combined edge, if this changed it.
    pub fn on_value(&mut self, id: u64, value: i32) -> Option<Edge> {
        match value {
            1 => (self.held.insert(id) && self.held.len() == 1).then_some(Edge::Pressed),
            0 => (self.held.remove(&id) && self.held.is_empty()).then_some(Edge::Released),
            _ => None, // autorepeat: a long hold is one press, not a storm
        }
    }

    /// Device `id` went away. If it was the last one holding the trigger,
    /// that is a release: its real release event is lost with it.
    pub fn forget(&mut self, id: u64) -> Option<Edge> {
        (self.held.remove(&id) && self.held.is_empty()).then_some(Edge::Released)
    }

    pub fn is_held(&self) -> bool {
        !self.held.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_press_one_release_autorepeat_ignored() {
        let mut s = TriggerState::default();
        assert_eq!(s.on_value(1, 1), Some(Edge::Pressed));
        for _ in 0..38 {
            assert_eq!(s.on_value(1, 2), None);
        }
        assert_eq!(s.on_value(1, 0), Some(Edge::Released));
        assert!(!s.is_held());
    }

    #[test]
    fn two_keyboards_overlap_into_one_hold() {
        let mut s = TriggerState::default();
        assert_eq!(s.on_value(1, 1), Some(Edge::Pressed));
        assert_eq!(s.on_value(2, 1), None);
        assert_eq!(s.on_value(1, 0), None, "still held on device 2");
        assert_eq!(s.on_value(2, 0), Some(Edge::Released));
    }

    #[test]
    fn unplugging_mid_hold_releases() {
        let mut s = TriggerState::default();
        s.on_value(7, 1);
        assert_eq!(s.forget(7), Some(Edge::Released));
        assert_eq!(s.forget(7), None);
        assert_eq!(s.forget(8), None, "an idle device leaving changes nothing");
    }

    #[test]
    fn a_stray_release_or_double_press_is_harmless() {
        let mut s = TriggerState::default();
        assert_eq!(s.on_value(1, 0), None);
        assert_eq!(s.on_value(1, 1), Some(Edge::Pressed));
        assert_eq!(s.on_value(1, 1), None);
        assert_eq!(s.on_value(1, 0), Some(Edge::Released));
    }
}
