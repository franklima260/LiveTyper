//! Shared display scale for the Scratch Pad's peak meter.

use std::time::Instant;

pub const SEGMENTS: usize = 10;
const COLORS: [[f32; 3]; 3] = [[0.29, 0.82, 0.49], [0.96, 0.71, 0.28], [0.94, 0.33, 0.34]];

const FLOOR_DBFS: f32 = -60.0;
const PEAK_DECAY_PER_SECOND: f32 = 6.0 / -FLOOR_DBFS;

#[derive(Clone, Copy)]
pub struct Segment {
    pub lit: bool,
    pub held: bool,
    pub color: [f32; 3],
}

#[derive(Default)]
pub struct PeakMeter {
    level: f32,
    held: f32,
    last_update: Option<Instant>,
}

impl PeakMeter {
    /// Updates the meter with the normalized peak from the latest block of microphone audio.
    /// Returns true when a lit segment or the peak marker changed.
    pub fn update(&mut self, level: f32) -> bool {
        self.update_at(level, Instant::now())
    }

    pub(crate) fn update_at(&mut self, level: f32, now: Instant) -> bool {
        let previous = (self.lit_count(), self.held_segment());
        let elapsed = self
            .last_update
            .replace(now)
            .map_or(0.0, |last| now.duration_since(last).as_secs_f32());

        self.level = level.clamp(0.0, 1.0);
        self.held = self.level.max(self.held - elapsed * PEAK_DECAY_PER_SECOND);

        previous != (self.lit_count(), self.held_segment())
    }

    /// Segments from bottom to top, with a marker at the most recent peak.
    pub fn segments(&self) -> impl Iterator<Item = Segment> {
        let lit_count = self.lit_count();
        let held = self.held_segment();
        (0..SEGMENTS).map(move |i| {
            let db = FLOOR_DBFS + (i + 1) as f32 * (-FLOOR_DBFS / SEGMENTS as f32);
            let zone = if db >= -3.0 {
                2
            } else if db >= -12.0 {
                1
            } else {
                0
            };
            Segment {
                lit: i < lit_count,
                held: Some(i) == held,
                color: COLORS[zone],
            }
        })
    }

    fn lit_count(&self) -> usize {
        (self.level * SEGMENTS as f32).round() as usize
    }

    fn held_segment(&self) -> Option<usize> {
        ((self.held * SEGMENTS as f32).round() as usize).checked_sub(1)
    }
}

pub fn normalized_peak(samples: &[f32]) -> f32 {
    let peak = samples
        .iter()
        .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
    let db = 20.0 * peak.log10();
    ((db - FLOOR_DBFS) / -FLOOR_DBFS).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn short_peak_uses_amplitude_and_decays_after_input_goes_quiet() {
        let mut meter = PeakMeter::default();
        let start = Instant::now();
        assert!(!meter.update_at(normalized_peak(&[0.0; 32]), start));
        assert!(meter.update_at(normalized_peak(&[0.0, -1.0, 0.0]), start));
        assert!(meter.segments().all(|segment| segment.lit));
        assert!(meter.segments().last().unwrap().held);

        assert!(meter.update_at(0.0, start + Duration::from_secs(1)));
        assert!(meter.segments().all(|segment| !segment.lit));
        assert!(meter.segments().nth(8).unwrap().held);

        assert!(meter.update_at(0.0, start + Duration::from_secs(11)));
        assert!(meter.segments().all(|segment| !segment.held));
    }
}
