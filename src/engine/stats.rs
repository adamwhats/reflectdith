//! Resource usage shown in the top-left overlay: process CPU and memory (Linux
//! `/proc`), GPU time per frame (timestamp queries), and wgpu's GPU memory.
use std::{
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::Instant,
};

// Frames whose GPU timings can be in flight at once. A frame is left untimed
// when every slot is still waiting to be read back.
const SLOTS: u32 = 3;
const FREE: u8 = 0;
const PENDING: u8 = 1;
const READY: u8 = 2;
const PAIR_BYTES: u64 = 2 * wgpu::QUERY_SIZE as u64;

/// The device features GPU timing needs, if the adapter offers them.
pub fn gpu_timing_features(adapter: &wgpu::Adapter) -> wgpu::Features {
    let wanted = wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS;
    if adapter.features().contains(wanted) {
        wanted
    } else {
        wgpu::Features::empty()
    }
}

struct Slot {
    resolve: wgpu::Buffer,
    readback: wgpu::Buffer,
    // Shared with the map callback, which runs once the GPU is done
    state: Arc<AtomicU8>,
}

/// Brackets each frame's command encoder with two GPU timestamps.
pub struct GpuTimer {
    queries: wgpu::QuerySet,
    slots: Vec<Slot>,
    // Nanoseconds per timestamp tick
    period: f64,
    // Slot written by the frame currently being encoded
    active: Option<u32>,
    total_ms: f64,
    frames: u32,
}

impl GpuTimer {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Option<Self> {
        if !device
            .features()
            .contains(wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS)
        {
            return None;
        }
        let queries = device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("Frame timestamps"),
            ty: wgpu::QueryType::Timestamp,
            count: 2 * SLOTS,
        });
        let buffer = |label, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: PAIR_BYTES,
                usage,
                mapped_at_creation: false,
            })
        };
        let slots = (0..SLOTS)
            .map(|_| Slot {
                resolve: buffer(
                    "Timestamp resolve",
                    wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                ),
                readback: buffer(
                    "Timestamp readback",
                    wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                ),
                state: Arc::new(AtomicU8::new(FREE)),
            })
            .collect();
        Some(Self {
            queries,
            slots,
            period: queue.get_timestamp_period() as f64,
            active: None,
            total_ms: 0.,
            frames: 0,
        })
    }

    /// Call right after creating the frame's encoder.
    pub fn begin(&mut self, encoder: &mut wgpu::CommandEncoder) {
        self.collect();
        self.active = self
            .slots
            .iter()
            .position(|slot| slot.state.load(Ordering::Acquire) == FREE)
            .map(|i| i as u32);
        if let Some(i) = self.active {
            encoder.write_timestamp(&self.queries, 2 * i);
        }
    }

    /// Call after the frame's last pass, before `encoder.finish()`.
    pub fn end(&mut self, encoder: &mut wgpu::CommandEncoder) {
        let Some(i) = self.active else { return };
        let slot = &self.slots[i as usize];
        encoder.write_timestamp(&self.queries, 2 * i + 1);
        encoder.resolve_query_set(&self.queries, 2 * i..2 * i + 2, &slot.resolve, 0);
        encoder.copy_buffer_to_buffer(&slot.resolve, 0, &slot.readback, 0, PAIR_BYTES);
    }

    /// Call after `queue.submit`, so the map waits on this frame's commands.
    pub fn submitted(&mut self) {
        let Some(i) = self.active.take() else { return };
        let slot = &self.slots[i as usize];
        slot.state.store(PENDING, Ordering::Release);
        let state = Arc::clone(&slot.state);
        slot.readback.map_async(wgpu::MapMode::Read, .., move |result| {
            // On failure, free the slot; the buffer was never mapped
            state.store(if result.is_ok() { READY } else { FREE }, Ordering::Release);
        });
    }

    // Accumulate every frame whose timestamps have been read back
    fn collect(&mut self) {
        for slot in &self.slots {
            if slot.state.load(Ordering::Acquire) != READY {
                continue;
            }
            {
                let view = slot.readback.get_mapped_range(..).expect("Timestamp buffer not mapped");
                let ticks: &[u64] = bytemuck::cast_slice(&view);
                // Timestamps can be unordered on some drivers; skip those
                if ticks[1] > ticks[0] {
                    self.total_ms += (ticks[1] - ticks[0]) as f64 * self.period / 1e6;
                    self.frames += 1;
                }
            }
            slot.readback.unmap();
            slot.state.store(FREE, Ordering::Release);
        }
    }

    /// Mean GPU milliseconds per timed frame since the last call.
    fn take_mean_ms(&mut self) -> Option<f64> {
        let mean = (self.frames > 0).then(|| self.total_ms / self.frames as f64);
        self.total_ms = 0.;
        self.frames = 0;
        mean
    }
}

/// Builds the overlay's stats text from deltas between one-second samples.
pub struct UsageStats {
    last_sample: Instant,
    last_cpu_seconds: Option<f64>,
}

impl UsageStats {
    pub fn new() -> Self {
        Self {
            last_sample: Instant::now(),
            last_cpu_seconds: process_cpu_seconds(),
        }
    }

    /// One "LABEL value" line per available stat, labels padded so the values
    /// line up in a monospace font. CPU is relative to one core, as in `top`,
    /// so can exceed 100%.
    pub fn sample(&mut self, fps: f32, device: &wgpu::Device, gpu_timer: Option<&mut GpuTimer>) -> String {
        let mut parts = vec![format!("FPS  {fps:.0}")];

        let elapsed = self.last_sample.elapsed().as_secs_f64();
        self.last_sample = Instant::now();
        let cpu_seconds = process_cpu_seconds();
        if let (Some(now), Some(before)) = (cpu_seconds, self.last_cpu_seconds) {
            parts.push(format!("CPU  {:.0}%", 100. * (now - before) / elapsed));
        }
        self.last_cpu_seconds = cpu_seconds;

        if let Some(bytes) = resident_bytes() {
            parts.push(format!("RAM  {}", mebibytes(bytes)));
        }
        if let Some(ms) = gpu_timer.and_then(GpuTimer::take_mean_ms) {
            parts.push(format!("GPU  {ms:.1} ms"));
        }
        if let Some(report) = device.generate_allocator_report() {
            parts.push(format!("VRAM {}", mebibytes(report.total_reserved_bytes)));
        }
        parts.join("\n")
    }
}

fn mebibytes(bytes: u64) -> String {
    format!("{:.0} MiB", bytes as f64 / (1024. * 1024.))
}

// User + system CPU time this process has used, from /proc/self/stat
#[cfg(target_os = "linux")]
fn process_cpu_seconds() -> Option<f64> {
    // Kernel-reported clock ticks; USER_HZ is 100 on all mainstream Linux ABIs
    const TICKS_PER_SECOND: f64 = 100.;
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    // Fields after the parenthesised command name, which may contain spaces.
    // utime and stime are fields 14 and 15, i.e. 12th and 13th after it.
    let mut fields = stat.rsplit_once(')')?.1.split_whitespace().skip(11);
    let user: f64 = fields.next()?.parse().ok()?;
    let system: f64 = fields.next()?.parse().ok()?;
    Some((user + system) / TICKS_PER_SECOND)
}

// Resident set size, from /proc/self/status
#[cfg(target_os = "linux")]
fn resident_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with("VmRSS:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib * 1024)
}

#[cfg(not(target_os = "linux"))]
fn process_cpu_seconds() -> Option<f64> {
    None
}

#[cfg(not(target_os = "linux"))]
fn resident_bytes() -> Option<u64> {
    None
}
