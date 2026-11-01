// Generates a blue noise texture with the void-and-cluster method (Ulichney,
// 1993), so there is no image file to ship. Each texel holds its rank in a
// pattern where nearby ranks are spread as evenly as possible, which makes a
// dither threshold with no visible structure. It tiles seamlessly as the
// distances wrap around the edges

// Width and height of the texture in texels
pub const SIZE: usize = 64;

// How far each point's influence spreads, in texels. Smaller packs the points
// closer to a grid, larger spreads them further apart
const SIGMA: f32 = 1.5;
// How many texels out the influence is calculated, past which it is negligible
const RADIUS: i32 = 6;

// A binary pattern of points together with how crowded each texel is by them
#[derive(Clone)]
struct Pattern {
    // Whether each texel holds a point
    points: Vec<bool>,
    // The summed influence of every point on each texel
    energy: Vec<f32>,
    // The influence of a point at each offset from it, (2 * RADIUS + 1) squared
    kernel: Vec<f32>,
}

impl Pattern {
    fn new() -> Self {
        let width = 2 * RADIUS + 1;
        let mut kernel = Vec::with_capacity((width * width) as usize);
        for dy in -RADIUS..=RADIUS {
            for dx in -RADIUS..=RADIUS {
                let distance_squared = (dx * dx + dy * dy) as f32;
                kernel.push((-distance_squared / (2.0 * SIGMA * SIGMA)).exp());
            }
        }
        Self {
            points: vec![false; SIZE * SIZE],
            energy: vec![0.0; SIZE * SIZE],
            kernel,
        }
    }

    // Add or remove a point, updating the energy around it
    fn set(&mut self, index: usize, on: bool) {
        self.points[index] = on;
        let sign = if on { 1.0 } else { -1.0 };
        let (x, y) = ((index % SIZE) as i32, (index / SIZE) as i32);
        let mut kernel_index = 0;
        for dy in -RADIUS..=RADIUS {
            for dx in -RADIUS..=RADIUS {
                let wrapped_x = (x + dx).rem_euclid(SIZE as i32) as usize;
                let wrapped_y = (y + dy).rem_euclid(SIZE as i32) as usize;
                self.energy[wrapped_y * SIZE + wrapped_x] += sign * self.kernel[kernel_index];
                kernel_index += 1;
            }
        }
    }

    // The point with the most crowding around it
    fn tightest_cluster(&self) -> usize {
        let mut best = None;
        for (index, &energy) in self.energy.iter().enumerate() {
            if self.points[index] && best.is_none_or(|(_, best_energy)| energy > best_energy) {
                best = Some((index, energy));
            }
        }
        best.expect("pattern has a point").0
    }

    // The empty texel with the least crowding around it
    fn largest_void(&self) -> usize {
        let mut best = None;
        for (index, &energy) in self.energy.iter().enumerate() {
            if !self.points[index] && best.is_none_or(|(_, best_energy)| energy < best_energy) {
                best = Some((index, energy));
            }
        }
        best.expect("pattern has an empty texel").0
    }
}

// A small deterministic random number generator, so the texture is the same
// every run
struct XorShift(u32);

impl XorShift {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0
    }
}

// Make the texture, SIZE by SIZE texels in rows, each holding its rank scaled
// to 0 to 255
pub fn generate() -> Vec<u8> {
    let texels = SIZE * SIZE;
    let mut rng = XorShift(0x9E37_79B9);

    // Start from random points covering about a tenth of the texture
    let initial_points = texels / 10;
    let mut pattern = Pattern::new();
    let mut placed = 0;
    while placed < initial_points {
        let index = rng.next() as usize % texels;
        if !pattern.points[index] {
            pattern.set(index, true);
            placed += 1;
        }
    }

    // Spread them out: move the most crowded point into the largest gap until
    // the best move is putting it back where it was. The limit is a safeguard
    // against two moves cancelling each other forever
    for _ in 0..texels {
        let cluster = pattern.tightest_cluster();
        pattern.set(cluster, false);
        let void = pattern.largest_void();
        pattern.set(void, true);
        if void == cluster {
            break;
        }
    }

    // The initial points get the lowest ranks. Take them away one at a time,
    // most crowded first, ranking from the top down so the last one left is 0
    let mut ranks = vec![0usize; texels];
    let mut removing = pattern.clone();
    for rank in (0..initial_points).rev() {
        let cluster = removing.tightest_cluster();
        removing.set(cluster, false);
        ranks[cluster] = rank;
    }

    // Every other texel is ranked by adding points to the largest gap, in
    // order, until the pattern is full
    for rank in initial_points..texels {
        let void = pattern.largest_void();
        pattern.set(void, true);
        ranks[void] = rank;
    }

    ranks.iter().map(|&rank| (rank * 255 / (texels - 1)) as u8).collect()
}
