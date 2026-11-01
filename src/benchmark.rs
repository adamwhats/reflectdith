//! Opt-in frame timings; normal rendering never waits for GPU completion.
use std::time::{Duration, Instant};

pub struct Benchmark {
    frames: usize,
    warmup: usize,
    samples: Vec<[f64; 3]>,
}

impl Benchmark {
    pub fn new(frames: usize) -> Self {
        Self {
            frames,
            warmup: 120,
            samples: Vec::with_capacity(frames),
        }
    }

    pub fn record(&mut self, prepare: Duration, encode: Duration, total: Duration) -> bool {
        if self.warmup > 0 {
            self.warmup -= 1;
            return false;
        }
        self.samples.push([
            prepare.as_secs_f64() * 1000.,
            encode.as_secs_f64() * 1000.,
            total.as_secs_f64() * 1000.,
        ]);
        if self.samples.len() < self.frames {
            return false;
        }
        for (index, label) in ["prepare_cpu", "encode_cpu", "work_gpu_complete"].iter().enumerate() {
            let mut values: Vec<_> = self.samples.iter().map(|s| s[index]).collect();
            values.sort_by(f64::total_cmp);
            let mean = values.iter().sum::<f64>() / values.len() as f64;
            eprintln!(
                "BENCH {label}: mean={mean:.3}ms p50={:.3}ms p95={:.3}ms n={}",
                values[values.len() / 2],
                values[(values.len() * 95 / 100).min(values.len() - 1)],
                values.len()
            );
        }
        true
    }
}

pub fn elapsed(start: Option<Instant>) -> Duration {
    start.map_or(Duration::ZERO, |s| s.elapsed())
}
