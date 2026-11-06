//! Display-driven time steps, after StereoKit's `frame_pacer`: the frame being
//! produced is predicted to show a refresh after the newest presentation
//! report per commit since, and a step is the gap between consecutive
//! predictions, so animation moves as far as the screen will show it moving.

#[derive(Clone, Copy, Debug)]
struct Anchor {
    /// The commit the report belongs to.
    seq: u64,
    display_ns: u64,
    /// A guess (the earliest the commit could show), not a report.
    floor: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Pacer {
    refresh_ns: u64,
    anchor: Option<Anchor>,
    predicted_ns: u64,
}

/// The step for the frame about to be produced.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Paced {
    /// When the frame is predicted to reach the screen (CLOCK_MONOTONIC), or
    /// 0 before the first report.
    pub display_ns: u64,
    /// The gap to the previous frame's prediction, or 0 where there is none
    /// (the caller uses the wall clock).
    pub dt_ns: u64,
}

impl Pacer {
    /// The output's refresh period; re-sent as the window moves between
    /// outputs.
    pub(crate) fn set_refresh(&mut self, refresh_ns: u64) {
        self.refresh_ns = refresh_ns;
    }
    pub(crate) fn refresh_ns(&self) -> u64 {
        self.refresh_ns
    }
    /// Commit `seq` reached the screen at `display_ns`.
    pub(crate) fn presented(&mut self, seq: u64, display_ns: u64) {
        let newer = self.anchor.is_none_or(|a| seq > a.seq || (a.floor && seq == a.seq));
        if newer {
            self.anchor = Some(Anchor { seq, display_ns, floor: false });
        }
    }
    /// The step for the frame whose commit will be `next_seq`, at `now`.
    pub(crate) fn next(&mut self, next_seq: u64, now: u64) -> Paced {
        let mut predicted = 0;
        if let Some(anchor) = self.anchor
            && self.refresh_ns > 0
        {
            predicted = anchor.display_ns + next_seq.saturating_sub(anchor.seq) * self.refresh_ns;
            // After a stall the anchor is stale, and the next commit can't
            // show before the refresh after now: anchoring there puts the
            // stall on this step instead of waiting for reports to settle.
            if now > predicted {
                predicted = now + self.refresh_ns - (now - anchor.display_ns) % self.refresh_ns;
                self.anchor = Some(Anchor { seq: next_seq, display_ns: predicted, floor: true });
            }
        }
        let dt_ns = if predicted != 0 && self.predicted_ns != 0 && predicted > self.predicted_ns {
            predicted - self.predicted_ns
        } else {
            0
        };
        self.predicted_ns = predicted;
        Paced { display_ns: predicted, dt_ns }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REFRESH: u64 = 8_333_333;

    #[test]
    fn steady_reports_step_one_refresh() {
        let mut p = Pacer::default();
        p.set_refresh(REFRESH);
        // Nothing known yet: the wall clock stands in.
        assert_eq!(p.next(1, 0), Paced::default());
        p.presented(1, 10 * REFRESH);
        let a = p.next(2, 10 * REFRESH + 1);
        assert_eq!(a.display_ns, 11 * REFRESH);
        assert_eq!(a.dt_ns, 0, "no previous prediction to measure from");
        p.presented(2, 11 * REFRESH);
        let b = p.next(3, 11 * REFRESH + 1);
        assert_eq!((b.display_ns, b.dt_ns), (12 * REFRESH, REFRESH));
    }

    #[test]
    fn a_stall_lands_on_the_next_step_from_the_refresh_after_now() {
        let mut p = Pacer::default();
        p.set_refresh(REFRESH);
        p.presented(1, 10 * REFRESH);
        p.next(2, 10 * REFRESH + 1);
        // Five refreshes pass without a report.
        let now = 15 * REFRESH + 100;
        let paced = p.next(3, now);
        assert_eq!(paced.display_ns, 16 * REFRESH);
        assert_eq!(paced.dt_ns, 16 * REFRESH - 11 * REFRESH);
        // The real report for that commit replaces the guess.
        p.presented(3, 17 * REFRESH);
        assert_eq!(p.next(4, 16 * REFRESH).display_ns, 18 * REFRESH);
        // An older report does not move the anchor back.
        p.presented(2, 11 * REFRESH);
        assert_eq!(p.next(4, 16 * REFRESH).display_ns, 18 * REFRESH);
    }

    #[test]
    fn unknown_refresh_predicts_nothing() {
        let mut p = Pacer::default();
        p.presented(1, 1_000);
        assert_eq!(p.next(2, 2_000), Paced::default());
    }
}
