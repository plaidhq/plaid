//! Standalone-press recognition for the dictation hotkey (port of HotkeyGesture in Hotkeys.swift).
//! Any chord invalidates the whole press; event timestamps, not delivery time, decide taps.
//! Platform layers supply key codes and two predicates: whether the event's key is a modifier, and whether
//! modifiers other than a given key's own are held.
use std::collections::HashSet;

pub const HOLD_DELAY: f64 = 0.3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    StartHold,
    StartToggle,
    Stop,
    Cancel,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Press {
    pub code: i32,
    /// Seconds, event clock.
    pub at: f64,
    pub ends_toggle: bool,
    pub other_key: bool,
    pub started: bool,
}

/// One key event as the platform sees it.
pub struct KeyEvent<'a> {
    pub code: i32,
    pub down: bool,
    pub at: f64,
    pub modifier: bool,
    pub repeat: bool,
    /// `other(code)`: modifiers other than `code`'s own family/side are held in this event's flags.
    pub other: &'a dyn Fn(i32) -> bool,
}

#[derive(Default)]
pub struct Gesture {
    press: Option<Press>,
    keys_down: HashSet<i32>,
    swallowed: HashSet<i32>,
}

impl Gesture {
    pub fn press(&self) -> Option<Press> {
        self.press
    }

    pub fn clear_press(&mut self) {
        self.press = None;
    }

    /// A chord after the hold threshold cancels without finalizing or pasting its audio.
    fn interrupt(&mut self) -> Vec<Action> {
        match &mut self.press {
            Some(p) if !p.other_key => {
                p.other_key = true;
                if p.started {
                    vec![Action::Cancel]
                } else {
                    vec![]
                }
            }
            _ => vec![],
        }
    }

    /// Called once the hold delay has passed since the press.
    pub fn advance(&mut self, at: f64, hold: i32) -> Vec<Action> {
        match &mut self.press {
            Some(p)
                if p.code == hold
                    && !p.other_key
                    && !p.ends_toggle
                    && !p.started
                    && at - p.at >= HOLD_DELAY =>
            {
                p.started = true;
                vec![Action::StartHold]
            }
            _ => vec![],
        }
    }

    /// Returns whether the event (a non-modifier down/up pair) belongs to Plaid and must be swallowed.
    pub fn key(&mut self, e: &KeyEvent, hold: i32, tap: i32, toggled: bool) -> (bool, Vec<Action>) {
        let code = e.code;
        let was_down = self.keys_down.contains(&code);
        if !e.modifier {
            if e.down {
                self.keys_down.insert(code);
            } else {
                self.keys_down.remove(&code);
            }
        }
        if self.press.is_some_and(|p| p.code != hold && p.code != tap) {
            self.press = None;
        }
        let mut actions = vec![];
        if e.down && self.press.is_some_and(|p| code != p.code) {
            actions.extend(self.interrupt());
        }
        if e.down && self.press.is_some_and(|p| (e.other)(p.code)) {
            actions.extend(self.interrupt());
        }
        let bound = code == hold || code == tap;
        if e.down && bound && !e.repeat && !was_down && self.press.is_none() {
            let chord = (e.other)(code) || self.keys_down.iter().any(|&k| k != code);
            self.press = Some(Press {
                code,
                at: e.at,
                ends_toggle: toggled,
                other_key: chord,
                started: false,
            });
            if !e.modifier && !chord {
                self.swallowed.insert(code);
            }
        }
        let swallow = !e.modifier && self.swallowed.contains(&code);
        if !e.down {
            self.swallowed.remove(&code);
            if let Some(p) = self.press.filter(|p| p.code == code) {
                self.press = None;
                if !p.other_key {
                    if p.ends_toggle {
                        actions.push(Action::Stop);
                    } else if code == tap && e.at - p.at < HOLD_DELAY {
                        actions.push(Action::StartToggle);
                    } else if p.started {
                        actions.push(Action::Stop);
                    }
                }
            }
        }
        (swallow, actions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const K: i32 = 54; // a modifier hotkey
    const F: i32 = 105; // a non-modifier hotkey (F13)
    const A: i32 = 0;

    fn ev(
        g: &mut Gesture,
        code: i32,
        down: bool,
        at: f64,
        other: bool,
        toggled: bool,
        hot: i32,
    ) -> (bool, Vec<Action>) {
        let o = move |_: i32| other;
        g.key(
            &KeyEvent {
                code,
                down,
                at,
                modifier: code == K,
                repeat: false,
                other: &o,
            },
            hot,
            hot,
            toggled,
        )
    }

    #[test]
    fn hold_starts_after_delay_and_stops_on_release() {
        let mut g = Gesture::default();
        assert_eq!(ev(&mut g, K, true, 0.0, false, false, K).1, vec![]);
        assert_eq!(g.advance(0.2, K), vec![]);
        assert_eq!(g.advance(0.31, K), vec![Action::StartHold]);
        assert_eq!(
            ev(&mut g, K, false, 2.0, false, false, K).1,
            vec![Action::Stop]
        );
    }

    #[test]
    fn quick_tap_toggles_and_next_press_stops() {
        let mut g = Gesture::default();
        ev(&mut g, K, true, 0.0, false, false, K);
        assert_eq!(
            ev(&mut g, K, false, 0.1, false, false, K).1,
            vec![Action::StartToggle]
        );
        ev(&mut g, K, true, 3.0, false, true, K);
        assert_eq!(
            g.advance(3.5, K),
            vec![],
            "a press that ends a toggle never starts a hold"
        );
        assert_eq!(
            ev(&mut g, K, false, 3.6, false, true, K).1,
            vec![Action::Stop]
        );
    }

    #[test]
    fn chord_before_start_is_ignored_after_start_cancels() {
        let mut g = Gesture::default();
        ev(&mut g, K, true, 0.0, false, false, K);
        assert_eq!(ev(&mut g, A, true, 0.1, false, false, K).1, vec![]);
        ev(&mut g, A, false, 0.15, false, false, K);
        assert_eq!(g.advance(0.4, K), vec![]);
        assert_eq!(ev(&mut g, K, false, 0.5, false, false, K).1, vec![]);

        ev(&mut g, K, true, 1.0, false, false, K);
        assert_eq!(g.advance(1.3, K), vec![Action::StartHold]);
        assert_eq!(
            ev(&mut g, A, true, 1.5, false, false, K).1,
            vec![Action::Cancel]
        );
        assert_eq!(ev(&mut g, K, false, 1.6, false, false, K).1, vec![]);
    }

    #[test]
    fn press_with_other_modifier_held_is_a_chord() {
        let mut g = Gesture::default();
        ev(&mut g, K, true, 0.0, true, false, K);
        assert_eq!(ev(&mut g, K, false, 0.1, true, false, K).1, vec![]);
    }

    #[test]
    fn non_modifier_hotkey_pair_is_swallowed() {
        let mut g = Gesture::default();
        assert!(ev(&mut g, F, true, 0.0, false, false, F).0);
        assert_eq!(
            ev(&mut g, F, false, 0.1, false, false, F),
            (true, vec![Action::StartToggle])
        );
        // Pressed while another key is held: a chord, passed through.
        ev(&mut g, A, true, 1.0, false, false, F);
        assert!(!ev(&mut g, F, true, 1.1, false, false, F).0);
    }
}
