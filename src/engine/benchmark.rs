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

/// GPU time between marks written around each pass, in benchmark mode only.
/// Mark 0 starts the frame; each later mark ends the pass named in `LABELS`.
pub struct PassTimer {
    queries: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    readback: wgpu::Buffer,
    // Nanoseconds per timestamp tick
    period: f64,
    warmup: usize,
    samples: Vec<[f64; PASSES]>,
}

const PASSES: usize = 4;
const LABELS: [&str; PASSES] = ["shadow", "scene", "post", "gui"];
const BYTES: u64 = (PASSES as u64 + 1) * wgpu::QUERY_SIZE as u64;

impl PassTimer {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Option<Self> {
        if !device
            .features()
            .contains(wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS)
        {
            return None;
        }
        let buffer = |label, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: BYTES,
                usage,
                mapped_at_creation: false,
            })
        };
        Some(Self {
            queries: device.create_query_set(&wgpu::QuerySetDescriptor {
                label: Some("Pass timestamps"),
                ty: wgpu::QueryType::Timestamp,
                count: PASSES as u32 + 1,
            }),
            resolve: buffer(
                "Pass timestamp resolve",
                wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            ),
            readback: buffer(
                "Pass timestamp readback",
                wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            ),
            period: queue.get_timestamp_period() as f64,
            warmup: 120,
            samples: Vec::new(),
        })
    }

    pub fn mark(&self, encoder: &mut wgpu::CommandEncoder, index: u32) {
        encoder.write_timestamp(&self.queries, index);
    }

    /// Call after the last mark, before `encoder.finish()`.
    pub fn resolve(&self, encoder: &mut wgpu::CommandEncoder) {
        encoder.resolve_query_set(&self.queries, 0..PASSES as u32 + 1, &self.resolve, 0);
        encoder.copy_buffer_to_buffer(&self.resolve, 0, &self.readback, 0, BYTES);
    }

    /// Call once the frame's GPU work is complete.
    pub fn read(&mut self, device: &wgpu::Device) {
        self.readback.map_async(wgpu::MapMode::Read, .., |result| {
            result.expect("Pass timestamp map failed");
        });
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("Pass timestamp wait failed");
        {
            let view = self.readback.get_mapped_range(..).expect("Pass timestamps not mapped");
            let ticks: &[u64] = bytemuck::cast_slice(&view);
            let mut sample = [0.; PASSES];
            for (pass, value) in sample.iter_mut().enumerate() {
                *value = ticks[pass + 1].saturating_sub(ticks[pass]) as f64 * self.period / 1e6;
            }
            if self.warmup > 0 {
                self.warmup -= 1;
            } else {
                self.samples.push(sample);
            }
        }
        self.readback.unmap();
    }

    pub fn report(&self) {
        for (pass, label) in LABELS.iter().enumerate() {
            let mut values: Vec<_> = self.samples.iter().map(|s| s[pass]).collect();
            values.sort_by(f64::total_cmp);
            let mean = values.iter().sum::<f64>() / values.len().max(1) as f64;
            eprintln!(
                "BENCH gpu_{label}: mean={mean:.3}ms p50={:.3}ms",
                values.get(values.len() / 2).copied().unwrap_or(0.)
            );
        }
    }
}
