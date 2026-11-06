//! Key repeat. The compositor sends one key event per physical press and
//! only states a rate and a delay (`wl_keyboard.repeat_info`), so repeats are
//! timed here: only the newest pressed key repeats, and after a stall the
//! schedule catches up without a burst (sk_app's `ska_wl_pump_key_repeat`).

const NS_PER_MS: u64 = 1_000_000;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Repeat {
    /// Repeats per second; 0 disables repeat.
    rate: u32,
    delay_ns: u64,
    /// The evdev code repeating, and when it next fires.
    key: Option<(u32, u64)>,
}

impl Default for Repeat {
    /// Until the compositor says otherwise: 25 per second after 600 ms, the
    /// X server's defaults.
    fn default() -> Self {
        Self { rate: 25, delay_ns: 600 * NS_PER_MS, key: None }
    }
}

impl Repeat {
    pub(crate) fn set_info(&mut self, rate: i32, delay_ms: i32) {
        self.rate = rate.max(0) as u32;
        self.delay_ns = delay_ms.max(0) as u64 * NS_PER_MS;
        if self.rate == 0 {
            self.key = None;
        }
    }
    /// `code` went down at `now`; `repeats` is the keymap's say (modifiers
    /// don't repeat).
    pub(crate) fn press(&mut self, code: u32, repeats: bool, now: u64) {
        self.key = (repeats && self.rate > 0).then_some((code, now + self.delay_ns));
    }
    /// `code` went up: it stops if it was the one repeating.
    pub(crate) fn release(&mut self, code: u32) {
        if self.key.is_some_and(|(k, _)| k == code) {
            self.key = None;
        }
    }
    /// Focus left: nothing repeats.
    pub(crate) fn clear(&mut self) {
        self.key = None;
    }
    /// When the next repeat fires.
    pub(crate) fn deadline(&self) -> Option<u64> {
        self.key.map(|(_, at)| at)
    }
    /// The key to repeat now, if one is due. At most one per call: a loop
    /// that stalled gets one repeat, and the next lands on the original
    /// cadence instead of a burst making up the missed ones.
    pub(crate) fn due(&mut self, now: u64) -> Option<u32> {
        let (code, at) = self.key?;
        if now < at {
            return None;
        }
        let interval = (1_000_000_000 / self.rate as u64).max(1);
        let overdue = now - at;
        self.key = Some((code, now + interval - overdue % interval));
        Some(code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = NS_PER_MS;

    #[test]
    fn repeats_after_the_delay_at_the_rate() {
        let mut r = Repeat::default();
        r.set_info(10, 300);
        r.press(30, true, 0);
        assert_eq!(r.due(299 * MS), None);
        assert_eq!(r.due(300 * MS), Some(30));
        assert_eq!(r.deadline(), Some(400 * MS));
        assert_eq!(r.due(399 * MS), None);
        assert_eq!(r.due(400 * MS), Some(30));
    }

    #[test]
    fn a_stall_gives_one_repeat_and_keeps_the_cadence() {
        let mut r = Repeat::default();
        r.set_info(10, 300);
        r.press(30, true, 0);
        // 1,050 ms: repeats were due at 300, 400, ... 1,000.
        assert_eq!(r.due(1_050 * MS), Some(30));
        assert_eq!(r.due(1_050 * MS), None);
        assert_eq!(r.deadline(), Some(1_100 * MS));
    }

    #[test]
    fn only_the_newest_key_repeats() {
        let mut r = Repeat::default();
        r.set_info(10, 300);
        r.press(30, true, 0);
        r.press(31, true, 100 * MS);
        // Releasing the older key leaves the newer one repeating.
        r.release(30);
        assert_eq!(r.due(400 * MS), Some(31));
        r.release(31);
        assert_eq!(r.deadline(), None);
    }

    #[test]
    fn modifiers_and_rate_zero_do_not_repeat() {
        let mut r = Repeat::default();
        r.set_info(10, 300);
        r.press(30, true, 0);
        // A modifier press stops the letter, as on every desktop.
        r.press(42, false, 50 * MS);
        assert_eq!(r.deadline(), None);
        r.press(30, true, 100 * MS);
        r.set_info(0, 300);
        assert_eq!(r.deadline(), None);
        r.press(30, true, 200 * MS);
        assert_eq!(r.due(10_000 * MS), None);
    }
}
