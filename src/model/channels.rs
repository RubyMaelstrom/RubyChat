//! The channel list (DESIGN.md §8.10): LIST's replies, kept as they stream
//! in, then filtered and sorted a slice at a time, so a list of a hundred
//! thousand channels never holds up a frame (DESIGN.md §10).
//!
//! Channels that arrive while the list is shown join it at once in its
//! order (by users), or, sorted by name, once the list is complete: the name
//! order is worked out once, and filtering then walks it.

use std::cmp::Ordering;
use std::time::{Duration, Instant};

use crate::irc::format;

use super::search::Query;

/// Channels kept, at most; more are counted.
pub const MAX_CHANNELS: usize = 200_000;
/// How long one wake may spend filtering and sorting before input gets a
/// turn (DESIGN.md §10).
pub const SLICE: Duration = Duration::from_millis(3);
/// Comparisons or tests between looks at the clock.
const STRIDE: usize = 256;
/// The merge sort starts from runs this long, sorted in place.
const RUN: usize = 32;

/// A channel as LIST describes it.
#[derive(Clone, Debug)]
pub struct Channel {
    /// The name, then the topic without formatting.
    text: Box<str>,
    split: u32,
    pub users: u32,
}

impl Channel {
    pub fn name(&self) -> &str {
        &self.text[..self.split as usize]
    }

    pub fn topic(&self) -> &str {
        &self.text[self.split as usize..]
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Order {
    /// The most users first.
    #[default]
    Users,
    Name,
}

/// Names compared ignoring ASCII case, then exactly.
fn by_name(a: &str, b: &str) -> Ordering {
    let (x, y) = (a.as_bytes(), b.as_bytes());
    for (p, q) in x.iter().zip(y) {
        let (p, q) = (p.to_ascii_lowercase(), q.to_ascii_lowercase());
        if p != q {
            return p.cmp(&q);
        }
    }
    x.len().cmp(&y.len()).then_with(|| x.cmp(y))
}

/// A stable merge sort of indices done a slice at a time: each `step`
/// stops at its deadline and the next carries on.
#[derive(Debug)]
pub struct SliceSort {
    items: Vec<u32>,
    scratch: Vec<u32>,
    /// The length of the sorted runs being merged in pairs; 0 while the
    /// first runs are sorted.
    width: usize,
    /// Where the pair being merged starts, and how far its left run, its
    /// right run and the output have got.
    lo: usize,
    i: usize,
    j: usize,
    k: usize,
}

impl SliceSort {
    pub fn new(items: Vec<u32>) -> Self {
        Self {
            scratch: vec![0; items.len()],
            items,
            width: 0,
            lo: 0,
            i: 0,
            j: 0,
            k: 0,
        }
    }

    /// Sorts until `deadline`; true once sorted.
    pub fn step(&mut self, deadline: Instant, mut order: impl FnMut(u32, u32) -> Ordering) -> bool {
        let n = self.items.len();
        let mut count = 0usize;
        while self.width == 0 {
            if self.lo >= n {
                self.width = RUN;
                self.start(0);
                break;
            }
            let end = (self.lo + RUN).min(n);
            self.items[self.lo..end].sort_by(|&a, &b| order(a, b));
            self.lo = end;
            count += RUN;
            if count.is_multiple_of(STRIDE) && Instant::now() >= deadline {
                return false;
            }
        }
        while self.width < n {
            let mid = (self.lo + self.width).min(n);
            let hi = (self.lo + 2 * self.width).min(n);
            while self.k < hi {
                // The left run's item first unless the right's sorts
                // before it: equal items keep their order.
                let left = self.j >= hi
                    || (self.i < mid && order(self.items[self.j], self.items[self.i]).is_ge());
                if left {
                    self.scratch[self.k] = self.items[self.i];
                    self.i += 1;
                } else {
                    self.scratch[self.k] = self.items[self.j];
                    self.j += 1;
                }
                self.k += 1;
                count += 1;
                if count.is_multiple_of(STRIDE) && Instant::now() >= deadline {
                    return false;
                }
            }
            if hi >= n {
                std::mem::swap(&mut self.items, &mut self.scratch);
                self.width *= 2;
                self.start(0);
            } else {
                self.start(hi);
            }
        }
        true
    }

    /// Starts merging the pair of runs at `lo`.
    fn start(&mut self, lo: usize) {
        self.lo = lo;
        self.i = lo;
        self.j = (lo + self.width).min(self.items.len());
        self.k = lo;
    }

    pub fn into_sorted(self) -> Vec<u32> {
        self.items
    }
}

/// Work left to bring the rows up to date.
#[derive(Debug)]
enum Work {
    /// Working out the name order.
    Naming(SliceSort),
    /// Testing channels against the filter, in the order filtering walks:
    /// the next place in it, where it ends, and the matches so far.
    Filtering {
        next: usize,
        end: usize,
        found: Vec<u32>,
    },
    /// Sorting the matches by users.
    Sorting(SliceSort),
}

#[derive(Debug, Default)]
pub struct ChannelList {
    channels: Vec<Channel>,
    /// LIST's replies are still coming.
    pub loading: bool,
    /// Channels past the cap, not kept.
    pub dropped: usize,
    /// Why the list ended early, if the server said.
    pub error: Option<String>,
    filter: Option<Query>,
    order: Order,
    /// The rows: channels by index, filtered and sorted.
    rows: Vec<u32>,
    /// Channels that arrived matching the filter, not yet among the rows.
    fresh: Vec<u32>,
    /// Every channel in name order, once the list is complete and it was
    /// wanted.
    named: Option<Vec<u32>>,
    work: Option<Work>,
}

impl ChannelList {
    /// A list on its way.
    pub fn loading() -> Self {
        Self {
            loading: true,
            ..Self::default()
        }
    }

    pub fn len(&self) -> usize {
        self.channels.len()
    }

    pub fn is_empty(&self) -> bool {
        self.channels.is_empty()
    }

    pub fn channel(&self, index: u32) -> Option<&Channel> {
        self.channels.get(index as usize)
    }

    /// The channels shown, by index, in order.
    pub fn rows(&self) -> &[u32] {
        &self.rows
    }

    pub fn order(&self) -> Order {
        self.order
    }

    /// Filtering or sorting is under way, or arrivals wait to join the
    /// rows: `step` has work to do.
    pub fn busy(&self) -> bool {
        self.work.is_some() || !self.fresh.is_empty()
    }

    /// Whether the rows are out of date beyond arrivals: what they show
    /// changes once `step` finishes.
    pub fn settling(&self) -> bool {
        self.work.is_some()
    }

    /// A channel from RPL_LIST.
    pub fn add(&mut self, name: &str, users: u32, topic: &str) {
        if self.channels.len() >= MAX_CHANNELS {
            self.dropped += 1;
            return;
        }
        // The topic as shown: formatting dropped and nothing invisible.
        let topic: String = format::strip(topic)
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        let mut text = String::with_capacity(name.len() + topic.len());
        text.push_str(name);
        text.push_str(&topic);
        let channel = Channel {
            text: text.into_boxed_str(),
            split: name.len() as u32,
            users,
        };
        let index = self.channels.len() as u32;
        if self.matches(&channel) {
            self.fresh.push(index);
        }
        self.channels.push(channel);
    }

    /// RPL_LISTEND, or the list ended early because of `error`.
    pub fn finish(&mut self, error: Option<String>) {
        if !self.loading {
            return;
        }
        self.loading = false;
        self.error = error;
        if self.order == Order::Name {
            self.restart();
        }
    }

    /// Shows only channels whose name or topic contains `text`.
    pub fn set_filter(&mut self, text: &str) {
        let filter = Query::new(text);
        if filter != self.filter {
            self.filter = filter;
            self.restart();
        }
    }

    pub fn set_order(&mut self, order: Order) {
        if order != self.order {
            self.order = order;
            self.restart();
        }
    }

    fn matches(&self, channel: &Channel) -> bool {
        self.filter
            .as_ref()
            .is_none_or(|query| query.is_in(channel.name()) || query.is_in(channel.topic()))
    }

    /// The rows are worked out anew: everything that arrived so far is
    /// tested again, and arrivals from now on are tested as they come.
    fn restart(&mut self) {
        self.fresh.clear();
        self.work = Some(
            if self.order == Order::Name && self.named.is_none() && !self.loading {
                Work::Naming(SliceSort::new((0..self.channels.len() as u32).collect()))
            } else {
                self.filtering()
            },
        );
    }

    fn filtering(&self) -> Work {
        Work::Filtering {
            next: 0,
            end: self.channels.len(),
            found: Vec::new(),
        }
    }

    /// Brings the rows up to date until `deadline`; true once they are.
    pub fn step(&mut self, deadline: Instant) -> bool {
        while let Some(work) = self.work.take() {
            match work {
                Work::Naming(mut sort) => {
                    let channels = &self.channels;
                    let done = sort.step(deadline, |a, b| {
                        by_name(channels[a as usize].name(), channels[b as usize].name())
                    });
                    if !done {
                        self.work = Some(Work::Naming(sort));
                        return false;
                    }
                    self.named = Some(sort.into_sorted());
                    self.work = Some(self.filtering());
                }
                Work::Filtering {
                    mut next,
                    end,
                    mut found,
                } => {
                    // By name once that order is known, else as they came.
                    while next < end {
                        let index = match &self.named {
                            Some(named) => named[next],
                            None => next as u32,
                        };
                        if self.matches(&self.channels[index as usize]) {
                            found.push(index);
                        }
                        next += 1;
                        if next.is_multiple_of(STRIDE) && Instant::now() >= deadline {
                            self.work = Some(Work::Filtering { next, end, found });
                            return false;
                        }
                    }
                    if self.order == Order::Users {
                        self.work = Some(Work::Sorting(SliceSort::new(found)));
                    } else {
                        self.rows = found;
                    }
                }
                Work::Sorting(mut sort) => {
                    let channels = &self.channels;
                    let done = sort.step(deadline, |a, b| {
                        channels[b as usize].users.cmp(&channels[a as usize].users)
                    });
                    if !done {
                        self.work = Some(Work::Sorting(sort));
                        return false;
                    }
                    self.rows = sort.into_sorted();
                }
            }
        }
        self.take_fresh();
        true
    }

    /// Arrivals join the rows: in place by users, else after them until
    /// the name order is known.
    fn take_fresh(&mut self) {
        if self.fresh.is_empty() {
            return;
        }
        let mut fresh = std::mem::take(&mut self.fresh);
        if self.order == Order::Name {
            self.rows.append(&mut fresh);
            return;
        }
        let channels = &self.channels;
        let users = |index: u32| channels[index as usize].users;
        fresh.sort_by_key(|&index| std::cmp::Reverse(users(index)));
        let mut merged = Vec::with_capacity(self.rows.len() + fresh.len());
        let (mut i, mut j) = (0, 0);
        while i < self.rows.len() && j < fresh.len() {
            // Earlier arrivals first among equals.
            if users(fresh[j]) > users(self.rows[i]) {
                merged.push(fresh[j]);
                j += 1;
            } else {
                merged.push(self.rows[i]);
                i += 1;
            }
        }
        merged.extend_from_slice(&self.rows[i..]);
        merged.extend_from_slice(&fresh[j..]);
        self.rows = merged;
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// A deadline already past: each step does the least it can.
    fn past() -> Instant {
        Instant::now() - Duration::from_secs(1)
    }

    fn names(list: &ChannelList) -> Vec<&str> {
        list.rows()
            .iter()
            .map(|&index| list.channel(index).unwrap().name())
            .collect()
    }

    fn finish(list: &mut ChannelList) -> usize {
        let mut steps = 1;
        while !list.step(past()) {
            steps += 1;
        }
        steps
    }

    #[test]
    fn slice_sorts_are_stable_and_finish_whatever_the_slices() {
        let mut seed = 0x2545_f491_u32;
        let keys: Vec<u32> = (0..5000)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                seed % 97
            })
            .collect();
        let mut expected: Vec<u32> = (0..keys.len() as u32).collect();
        expected.sort_by_key(|&index| keys[index as usize]);
        for deadline in [past(), Instant::now() + Duration::from_secs(60)] {
            let mut sort = SliceSort::new((0..keys.len() as u32).collect());
            let mut steps = 0;
            while !sort.step(deadline, |a, b| keys[a as usize].cmp(&keys[b as usize])) {
                steps += 1;
            }
            assert_eq!(sort.into_sorted(), expected, "after {steps} more steps");
        }
        for n in [0, 1, RUN, RUN + 1, 3 * RUN - 1] {
            let mut sort = SliceSort::new((0..n as u32).rev().collect());
            while !sort.step(past(), |a, b| a.cmp(&b)) {}
            assert_eq!(sort.into_sorted(), (0..n as u32).collect::<Vec<_>>());
        }
    }

    #[test]
    fn arrivals_take_their_place_by_users() {
        let mut list = ChannelList::loading();
        list.add("#small", 3, "a \u{2}bold\u{2} topic");
        list.add("#big", 300, "");
        assert!(list.busy());
        finish(&mut list);
        assert_eq!(names(&list), ["#big", "#small"]);
        assert_eq!(list.channel(0).unwrap().topic(), "a bold topic");
        list.add("#middle", 30, "");
        list.add("#also-small", 3, "");
        finish(&mut list);
        assert_eq!(names(&list), ["#big", "#middle", "#small", "#also-small"]);
    }

    #[test]
    fn filtering_and_name_order_work_in_slices() {
        let mut list = ChannelList::loading();
        for n in 0..2000 {
            let topic = if n % 10 == 0 { "about Rust" } else { "chat" };
            list.add(&format!("#c{n:04}"), n % 50, topic);
        }
        list.add("#RUSTaceans", 7, "");
        list.finish(None);
        finish(&mut list);
        assert_eq!(list.rows().len(), 2001);
        list.set_filter("rust");
        assert!(list.settling());
        assert!(finish(&mut list) > 1, "a slice at a time");
        assert_eq!(list.rows().len(), 201);
        let users: Vec<u32> = list
            .rows()
            .iter()
            .map(|&index| list.channel(index).unwrap().users)
            .collect();
        assert!(users.windows(2).all(|pair| pair[0] >= pair[1]));
        list.set_order(Order::Name);
        finish(&mut list);
        let shown = names(&list);
        assert_eq!(shown[..3], ["#c0000", "#c0010", "#c0020"]);
        assert_eq!(shown.last(), Some(&"#RUSTaceans"));
        // The name order is kept: filtering again needs no sorting.
        list.set_filter("RUSTA");
        assert!(list.step(Instant::now() + Duration::from_secs(60)));
        assert_eq!(names(&list), ["#RUSTaceans"]);
    }

    #[test]
    fn the_cap_counts_what_it_drops() {
        let mut list = ChannelList::loading();
        list.channels = vec![
            Channel {
                text: "#x".into(),
                split: 2,
                users: 1,
            };
            MAX_CHANNELS
        ];
        list.add("#one-too-many", 1, "");
        assert_eq!((list.len(), list.dropped), (MAX_CHANNELS, 1));
        list.finish(Some(String::from("Server load is too heavy")));
        list.finish(None);
        assert_eq!(list.error.as_deref(), Some("Server load is too heavy"));
    }
}
