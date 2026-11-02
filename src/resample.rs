//! A streaming resampler: windowed-sinc low-pass and interpolation, from any rate to 16 kHz.
//!
//! The microphone usually gives 48 or 44.1 kHz. Speech models need 16 kHz. Without the low-pass,
//! sound above 8 kHz would fold back into the speech band.

use std::f64::consts::PI;

pub struct Resampler {
    /// Input samples per output sample.
    step: f64,
    /// Cut-off frequency in cycles per input sample.
    cutoff: f64,
    /// Filter half-width in input samples.
    half: f64,
    /// Input samples that the next outputs still need.
    history: Vec<f32>,
    /// Position of the next output sample, in `history` index units.
    position: f64,
}

impl Resampler {
    pub fn new(input_rate: u32, output_rate: u32) -> Self {
        let step = input_rate as f64 / output_rate as f64;
        // Keep a small margin below the output Nyquist frequency.
        let cutoff = 0.5 * (1.0 / step).min(1.0) * 0.92;
        Resampler {
            step,
            cutoff,
            half: (8.0 * step.max(1.0)).ceil(),
            history: Vec::new(),
            position: 0.0,
        }
    }

    pub fn process(&mut self, input: &[f32], output: &mut Vec<f32>) {
        if self.step == 1.0 {
            output.extend_from_slice(input);
            return;
        }
        self.history.extend_from_slice(input);
        while self.position + self.half < self.history.len() as f64 {
            let center = self.position;
            let first = (center - self.half).ceil().max(0.0) as usize;
            let last = (center + self.half).floor() as usize;
            let mut sum = 0.0f64;
            for (k, &x) in self.history[first..=last].iter().enumerate() {
                let t = center - (first + k) as f64;
                sum += x as f64 * self.kernel(t);
            }
            output.push(sum as f32);
            self.position += self.step;
        }
        let consumed = (self.position - self.half).floor().max(0.0) as usize;
        let consumed = consumed.min(self.history.len());
        self.history.drain(..consumed);
        self.position -= consumed as f64;
    }

    /// Low-pass impulse response at `t` input samples from the center, with a Blackman window.
    fn kernel(&self, t: f64) -> f64 {
        let x = 2.0 * self.cutoff * t;
        let sinc = if x == 0.0 {
            1.0
        } else {
            (PI * x).sin() / (PI * x)
        };
        let u = t / self.half; // -1..1
        let blackman = 0.42 + 0.5 * (PI * u).cos() + 0.08 * (2.0 * PI * u).cos();
        2.0 * self.cutoff * sinc * blackman
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f64, rate: u32, seconds: f64) -> Vec<f32> {
        (0..(rate as f64 * seconds) as usize)
            .map(|i| (2.0 * PI * freq * i as f64 / rate as f64).sin() as f32)
            .collect()
    }

    fn peak(samples: &[f32]) -> f32 {
        // Skip the start, where the filter has no history yet.
        samples[samples.len() / 4..]
            .iter()
            .fold(0.0f32, |m, s| m.max(s.abs()))
    }

    fn resample_in_blocks(input: &[f32], from: u32, block: usize) -> Vec<f32> {
        let mut r = Resampler::new(from, 16_000);
        let mut out = Vec::new();
        for chunk in input.chunks(block) {
            r.process(chunk, &mut out);
        }
        out
    }

    #[test]
    fn length_follows_the_rate_ratio() {
        for from in [48_000, 44_100, 32_000] {
            let out = resample_in_blocks(&sine(440.0, from, 1.0), from, 480);
            let expected = 16_000.0;
            assert!(
                (out.len() as f64 - expected).abs() < 40.0,
                "{from}: {}",
                out.len()
            );
        }
    }

    #[test]
    fn speech_band_keeps_its_level() {
        for from in [48_000, 44_100] {
            let out = resample_in_blocks(&sine(1000.0, from, 1.0), from, 441);
            let p = peak(&out);
            assert!((0.95..1.05).contains(&p), "{from}: peak {p}");
        }
    }

    #[test]
    fn sound_above_8_khz_is_removed() {
        let out = resample_in_blocks(&sine(11_000.0, 48_000, 1.0), 48_000, 480);
        assert!(peak(&out) < 0.02, "peak {}", peak(&out));
    }

    #[test]
    fn same_rate_passes_through() {
        let input = sine(440.0, 16_000, 0.1);
        assert_eq!(resample_in_blocks(&input, 16_000, 100), input);
    }
}
