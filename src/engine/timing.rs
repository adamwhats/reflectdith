use std::time::{Duration, Instant};

// Keep the original cadence after a late wake-up. Skip elapsed slots rather
// than shifting every later frame or rendering a burst of catch-up frames.
pub fn next_deadline(previous: Instant, now: Instant, interval: Duration) -> Instant {
    let slots = now.saturating_duration_since(previous).as_nanos() / interval.as_nanos() + 1;
    previous + interval.mul_f64(slots as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn late_wakes_do_not_accumulate_drift() {
        let start = Instant::now();
        let interval = Duration::from_secs_f64(1. / 60.);
        let mut deadline = start;
        for frame in 1..=600 {
            let late = deadline + Duration::from_micros(700);
            deadline = next_deadline(deadline, late, interval);
            assert_eq!(deadline, start + interval * frame);
            assert!(deadline > late);
        }
    }

    #[test]
    fn a_long_stall_skips_missed_slots_without_a_catch_up_burst() {
        let start = Instant::now();
        let interval = Duration::from_millis(10);
        for (late_ms, next_ms) in [(0, 10), (9, 10), (10, 20), (253, 260), (1000, 1010)] {
            let now = start + Duration::from_millis(late_ms);
            let next = next_deadline(start, now, interval);
            assert_eq!(next, start + Duration::from_millis(next_ms));
            assert!(next > now && next - now <= interval);
        }
    }
}
