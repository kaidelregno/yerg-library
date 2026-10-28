//! The play queue: a list the user started from, optional shuffle, repeat
//! modes and a "play next" lane layered on top.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};
use yerg_core::model::TrackId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Repeat {
    #[default]
    Off,
    All,
    One,
}

impl Repeat {
    pub fn next(self) -> Repeat {
        match self {
            Repeat::Off => Repeat::All,
            Repeat::All => Repeat::One,
            Repeat::One => Repeat::Off,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Repeat::Off => "off",
            Repeat::All => "all",
            Repeat::One => "one",
        }
    }
    pub fn parse(s: &str) -> Repeat {
        match s {
            "all" => Repeat::All,
            "one" => Repeat::One,
            _ => Repeat::Off,
        }
    }
}

#[derive(Debug, Default)]
pub struct Queue {
    /// The list as the user saw it.
    original: Vec<TrackId>,
    /// Actual play order (equals `original` unless shuffled).
    order: Vec<TrackId>,
    pos: Option<usize>,
    /// Tracks queued with "Play Next"; consumed before `order` advances.
    up_next: VecDeque<TrackId>,
    pub shuffle: bool,
    pub repeat: Repeat,
    rng: u64,
}

impl Queue {
    pub fn new(shuffle: bool, repeat: Repeat) -> Queue {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9E37_79B9_7F4A_7C15)
            | 1;
        Queue {
            shuffle,
            repeat,
            rng: seed,
            ..Default::default()
        }
    }

    fn rand(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn shuffled(&mut self, mut items: Vec<TrackId>) -> Vec<TrackId> {
        for i in (1..items.len()).rev() {
            let j = (self.rand() % (i as u64 + 1)) as usize;
            items.swap(i, j);
        }
        items
    }

    /// Replace the queue with `items` and start at `start`.
    pub fn set(&mut self, items: Vec<TrackId>, start: usize) -> Option<TrackId> {
        self.original = items;
        if self.original.is_empty() {
            self.order.clear();
            self.pos = None;
            return None;
        }
        let start = start.min(self.original.len() - 1);
        let first = self.original[start];
        if self.shuffle {
            let rest: Vec<TrackId> = self
                .original
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != start)
                .map(|(_, t)| *t)
                .collect();
            let mut order = vec![first];
            order.extend(self.shuffled(rest));
            self.order = order;
            self.pos = Some(0);
        } else {
            self.order = self.original.clone();
            self.pos = Some(start);
        }
        Some(first)
    }

    pub fn current(&self) -> Option<TrackId> {
        self.pos.and_then(|p| self.order.get(p).copied())
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty() && self.up_next.is_empty()
    }

    /// Advance.  `manual` is a user skip (ignores repeat-one).
    pub fn next(&mut self, manual: bool) -> Option<TrackId> {
        if let Some(t) = self.up_next.pop_front() {
            return Some(t);
        }
        if self.order.is_empty() {
            return None;
        }
        if self.repeat == Repeat::One && !manual {
            return self.current();
        }
        let p = self.pos.map(|p| p + 1).unwrap_or(0);
        if p < self.order.len() {
            self.pos = Some(p);
            return self.current();
        }
        match self.repeat {
            Repeat::Off => {
                self.pos = None;
                None
            }
            _ => {
                if self.shuffle {
                    let items = self.original.clone();
                    self.order = self.shuffled(items);
                }
                self.pos = Some(0);
                self.current()
            }
        }
    }

    pub fn prev(&mut self) -> Option<TrackId> {
        if self.order.is_empty() {
            return None;
        }
        match self.pos {
            Some(p) if p > 0 => {
                self.pos = Some(p - 1);
            }
            Some(_) => {
                if self.repeat == Repeat::All {
                    self.pos = Some(self.order.len() - 1);
                }
            }
            None => self.pos = Some(self.order.len() - 1),
        }
        self.current()
    }

    pub fn set_shuffle(&mut self, on: bool) {
        if on == self.shuffle {
            return;
        }
        self.shuffle = on;
        let current = self.current();
        if on {
            let rest: Vec<TrackId> = self.original.iter().copied().filter(|t| Some(*t) != current).collect();
            let mut order = Vec::with_capacity(self.original.len());
            if let Some(c) = current {
                order.push(c);
            }
            order.extend(self.shuffled(rest));
            self.order = order;
            self.pos = current.map(|_| 0);
        } else {
            self.order = self.original.clone();
            self.pos = current.and_then(|c| self.order.iter().position(|t| *t == c));
        }
    }

    pub fn play_next(&mut self, ids: &[TrackId]) {
        for id in ids.iter().rev() {
            self.up_next.push_front(*id);
        }
    }

    pub fn add_to_queue(&mut self, ids: &[TrackId]) {
        self.up_next.extend(ids.iter().copied());
    }

    /// Everything still to come, "play next" lane first.
    pub fn upcoming(&self) -> Vec<TrackId> {
        let mut out: Vec<TrackId> = self.up_next.iter().copied().collect();
        if let Some(p) = self.pos {
            out.extend(self.order.iter().skip(p + 1).copied());
        }
        out
    }

    /// Jump to an entry of [`upcoming`](Self::upcoming) by position.
    pub fn jump_upcoming(&mut self, i: usize) -> Option<TrackId> {
        if i < self.up_next.len() {
            let id = self.up_next.remove(i);
            // Drop everything before it in the lane
            for _ in 0..i {
                self.up_next.pop_front();
            }
            return id;
        }
        let i = i - self.up_next.len();
        self.up_next.clear();
        let p = self.pos.map(|p| p + 1 + i).unwrap_or(i);
        if p < self.order.len() {
            self.pos = Some(p);
            self.current()
        } else {
            None
        }
    }

    pub fn remove_upcoming(&mut self, i: usize) {
        if i < self.up_next.len() {
            self.up_next.remove(i);
            return;
        }
        let i = i - self.up_next.len();
        if let Some(p) = self.pos {
            let idx = p + 1 + i;
            if idx < self.order.len() {
                let id = self.order.remove(idx);
                if let Some(op) = self.original.iter().rposition(|t| *t == id) {
                    self.original.remove(op);
                }
            }
        }
    }

    /// Drag and drop: move the [`upcoming`](Self::upcoming) entry at `from` so
    /// it lands before the entry now at `to` (`to == len` puts it last).
    /// Afterwards `upcoming()` is exactly the old list with that one entry
    /// moved, and tracks play in that order.
    pub fn move_upcoming(&mut self, from: usize, to: usize) {
        let lane = self.up_next.len();
        let rest = self.pos.map(|p| self.order.len().saturating_sub(p + 1)).unwrap_or(0);
        let len = lane + rest;
        if from >= len || to > len || to == from || to == from + 1 {
            return;
        }
        // Take the entry out.
        let from_lane = from < lane;
        let id = if from_lane {
            self.up_next.remove(from).expect("index checked")
        } else {
            let idx = self.pos.expect("order entries need a position") + 1 + (from - lane);
            let id = self.order.remove(idx);
            if self.shuffle {
                if let Some(op) = self.original.iter().rposition(|t| *t == id) {
                    self.original.remove(op);
                }
            }
            id
        };
        let to = if to > from { to - 1 } else { to };
        // Put it back: in the lane when it lands inside it (or at its end and
        // came from it), otherwise into the play order.
        let lane = self.up_next.len();
        if to < lane || (to == lane && from_lane) || self.pos.is_none() {
            self.up_next.insert(to.min(lane), id);
        } else {
            let idx = self.pos.expect("checked above") + 1 + (to - lane);
            self.order.insert(idx, id);
            if self.shuffle {
                self.original.push(id);
            }
        }
        if !self.shuffle {
            // Unshuffled, the play order is the list itself.
            self.original = self.order.clone();
        }
    }

    pub fn clear_upcoming(&mut self) {
        self.up_next.clear();
        if let Some(p) = self.pos {
            self.order.truncate(p + 1);
            self.original = self.order.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_and_repeat() {
        let mut q = Queue::new(false, Repeat::Off);
        assert_eq!(q.set(vec![1, 2, 3], 1), Some(2));
        assert_eq!(q.next(false), Some(3));
        assert_eq!(q.next(false), None);
        q.repeat = Repeat::All;
        q.set(vec![1, 2, 3], 2);
        assert_eq!(q.next(false), Some(1));
        q.repeat = Repeat::One;
        assert_eq!(q.next(false), Some(1));
        assert_eq!(q.next(true), Some(2));
        assert_eq!(q.prev(), Some(1));
    }

    #[test]
    fn play_next_lane_and_shuffle() {
        let mut q = Queue::new(true, Repeat::Off);
        let first = q.set((1..=20).collect(), 5).unwrap();
        assert_eq!(first, 6);
        assert_eq!(q.len(), 20);
        q.play_next(&[100, 101]);
        assert_eq!(q.upcoming()[..2], [100, 101]);
        assert_eq!(q.next(false), Some(100));
        assert_eq!(q.next(false), Some(101));
        let n = q.next(false).unwrap();
        assert!((1..=20).contains(&n) && n != 6);
        q.set_shuffle(false);
        assert_eq!(q.current(), Some(n));
        assert!(!q.is_empty());
        // Back in linear order the next track follows the current one
        let expected = if n < 20 { Some(n + 1) } else { None };
        assert_eq!(q.next(false), expected);
    }

    /// A queue with a "play next" lane of two and four more tracks to come.
    fn mixed(shuffle: bool) -> Queue {
        let mut q = Queue::new(shuffle, Repeat::Off);
        q.set(vec![1, 2, 3, 4, 5, 6], 1);
        q.play_next(&[100, 101]);
        q
    }

    #[test]
    fn every_move_matches_a_plain_list_move() {
        for shuffle in [false, true] {
            let len = mixed(shuffle).upcoming().len();
            for from in 0..len {
                for to in 0..=len {
                    let mut q = mixed(shuffle);
                    // Same seed is not guaranteed, so compare against this queue's own list.
                    let mut expected = q.upcoming();
                    let item = expected.remove(from);
                    expected.insert(if to > from { to - 1 } else { to }, item);
                    q.move_upcoming(from, to);
                    assert_eq!(q.upcoming(), expected, "shuffle={shuffle} from={from} to={to}");
                    // And it plays in that order.
                    let played: Vec<TrackId> = std::iter::from_fn(|| q.next(false)).collect();
                    assert_eq!(played, expected, "shuffle={shuffle} from={from} to={to}");
                }
            }
        }
    }

    #[test]
    fn moves_survive_turning_shuffle_off() {
        let mut q = Queue::new(false, Repeat::Off);
        q.set(vec![1, 2, 3, 4, 5], 0);
        q.move_upcoming(3, 0); // 5 before 2
        assert_eq!(q.upcoming(), vec![5, 2, 3, 4]);
        q.set_shuffle(true);
        q.set_shuffle(false);
        // The user's order is kept as the unshuffled list.
        assert_eq!(q.upcoming(), vec![5, 2, 3, 4]);
    }

    #[test]
    fn moving_into_and_out_of_the_lane_keeps_every_track_once() {
        let mut q = mixed(true);
        let before = {
            let mut v = q.upcoming();
            v.sort();
            v
        };
        let sorted = |v: &[TrackId]| {
            let mut v = v.to_vec();
            v.sort();
            v
        };
        // [100, 101, a, b, c, d] -> 100 moves into the play order before d.
        q.move_upcoming(0, 5);
        assert_eq!(q.upcoming()[4], 100);
        assert_eq!(sorted(&q.upcoming()), before);
        assert_eq!(sorted(&q.original), vec![1, 2, 3, 4, 5, 6, 100]);
        // ...and back to the front, into the "play next" lane again.
        q.move_upcoming(4, 0);
        assert_eq!(q.upcoming()[0], 100);
        assert_eq!(sorted(&q.upcoming()), before);
        assert_eq!(sorted(&q.original), vec![1, 2, 3, 4, 5, 6]);
        // Turning shuffle off: the lane, then the list in order after track 2.
        q.set_shuffle(false);
        assert_eq!(q.upcoming(), vec![100, 101, 3, 4, 5, 6]);
    }
}
