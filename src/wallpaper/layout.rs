//! Logical desktop coordinates, independent of monitor resolution and DPI.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Desktop {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Desktop {
    pub fn new(rects: impl Iterator<Item = Rect>) -> Option<Self> {
        let mut rects = rects.filter(|r| r.width > 0 && r.height > 0);
        let first = rects.next()?;
        let mut x = first.x as f64;
        let mut y = first.y as f64;
        let mut right = x + first.width as f64;
        let mut bottom = y + first.height as f64;
        for rect in rects {
            x = x.min(rect.x as f64);
            y = y.min(rect.y as f64);
            right = right.max(rect.x as f64 + rect.width as f64);
            bottom = bottom.max(rect.y as f64 + rect.height as f64);
        }
        Some(Self {
            x,
            y,
            width: right - x,
            height: bottom - y,
        })
    }
    pub fn crop(&self, rect: Rect) -> [f32; 4] {
        [
            ((rect.x as f64 - self.x) / self.width) as f32,
            ((rect.y as f64 - self.y) / self.height) as f32,
            (rect.width as f64 / self.width) as f32,
            (rect.height as f64 / self.height) as f32,
        ]
    }
}

/// Uniformly reduce an oversized virtual desktop to the GPU texture limit.
pub fn texture_size(width: f64, height: f64, scale: i32, limit: u32) -> (u32, u32) {
    let width = width.max(1.) * scale.max(1) as f64;
    let height = height.max(1.) * scale.max(1) as f64;
    let reduction = (limit as f64 / width.max(height)).min(1.);
    (
        (width * reduction).round().max(1.) as u32,
        (height * reduction).round().max(1.) as u32,
    )
}

/// Fit a duplicated image without stretching it on differently shaped monitors.
pub fn fit(source: (u32, u32), target: (u32, u32)) -> [f32; 4] {
    let source_aspect = source.0 as f32 / source.1 as f32;
    let target_aspect = target.0 as f32 / target.1 as f32;
    if source_aspect > target_aspect {
        let height = target_aspect / source_aspect;
        [0., (1. - height) / 2., 1., height]
    } else {
        let width = source_aspect / target_aspect;
        [(1. - width) / 2., 0., width, 1.]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn adjacent_monitors_share_exactly_one_seam() {
        let left = Rect {
            x: -1920,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let right = Rect { x: 0, ..left };
        let desktop = Desktop::new([left, right].into_iter()).unwrap();
        assert_eq!(desktop.crop(left), [0., 0., 0.5, 1.]);
        assert_eq!(desktop.crop(right), [0.5, 0., 0.5, 1.]);
    }
    #[test]
    fn vertical_offsets_and_gaps_are_preserved() {
        let top = Rect {
            x: 0,
            y: -1200,
            width: 1920,
            height: 1080,
        };
        let bottom = Rect {
            x: 240,
            y: 0,
            width: 1080,
            height: 1920,
        };
        let desktop = Desktop::new([top, bottom].into_iter()).unwrap();
        assert_eq!(desktop.height, 3120.);
        assert_eq!(desktop.crop(bottom)[0], 0.125);
        assert!((desktop.crop(bottom)[1] - 1200. / 3120.).abs() < 1e-6);
        assert!(Desktop::new(std::iter::empty()).is_none());
    }
    #[test]
    fn mixed_dpi_and_large_layouts_keep_their_aspect() {
        assert_eq!(texture_size(3840., 1080., 2, 8192), (7680, 2160));
        assert_eq!(texture_size(12000., 3000., 2, 8192), (8192, 2048));
        assert_eq!(fit((1920, 1080), (1080, 1920)), [0., 0.34179688, 1., 0.31640625]);
    }
}
