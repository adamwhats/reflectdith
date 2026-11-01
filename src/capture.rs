use std::{
    error::Error,
    path::PathBuf,
    sync::mpsc,
    time::{SystemTime, UNIX_EPOCH},
};

use wgpu::{
    BufferDescriptor, BufferUsages, Extent3d, MapMode, Origin3d, PollType, TexelCopyBufferInfo, TexelCopyBufferLayout,
    TexelCopyTextureInfo, TextureAspect, TextureDescriptor, TextureDimension, TextureFormat, TextureUsages,
};

use crate::{WgpuContext, camera, textures};

// How many times larger than the window the screenshot is, in each direction.
// The window is made 1/CAPTURE_SCALE of the monitor, so a screenshot is exactly
// the monitor's resolution. It is reduced if the GPU can't make a texture or
// buffer that big
pub const CAPTURE_SCALE: u32 = 2;

// Directory the screenshots are saved in
const CAPTURE_DIR: &str = "screenshots";

impl WgpuContext {
    // Render the current frame again at a higher resolution, without the menu,
    // and save it as a PNG. Returns the path it was saved to and the size of the image
    //
    // The dither cells are scaled up with the image, so the screenshot looks
    // like the window but sharper. The commands are submitted straight away
    // and this waits for the GPU to finish
    pub fn save_screenshot(&self, orbit: &camera::Orbit) -> Result<(PathBuf, u32, u32), Box<dyn Error>> {
        let device = &self.device;
        let queue = &self.queue;
        let format = self.surface_config.format;
        let window_width = self.surface_config.width;
        let window_height = self.surface_config.height;

        // The bytes are copied out as 8-bit RGBA or BGRA, which is what the
        // surface format is on every platform seen so far
        let swap_red_blue = match format {
            TextureFormat::Rgba8Unorm | TextureFormat::Rgba8UnormSrgb => false,
            TextureFormat::Bgra8Unorm | TextureFormat::Bgra8UnormSrgb => true,
            other => return Err(format!("unsupported surface format {other:?}").into()),
        };

        // Each row of the texture copy has to be padded to a multiple of 256
        // bytes
        let padded_bytes_per_row = |width: u32| (width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);

        // Pick the largest scale up to CAPTURE_SCALE which fits the limits
        let limits = device.limits();
        let mut scale = CAPTURE_SCALE
            .min(limits.max_texture_dimension_2d / window_width.max(window_height))
            .max(1);
        while scale > 1
            && padded_bytes_per_row(window_width * scale) as u64 * (window_height * scale) as u64
                > limits.max_buffer_size
        {
            scale -= 1;
        }
        let width = window_width * scale;
        let height = window_height * scale;

        // The targets for the scene pass, and the image the post pass makes,
        // which is also copied out so it needs COPY_SRC
        let scene_view = textures::create_scene_view(device, width, height);
        let depth_view = textures::create_depth_view(device, width, height);
        let size = Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let output_texture = device.create_texture(&TextureDescriptor {
            label: Some("Capture texture"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format,
            usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let output_view = output_texture.create_view(&wgpu::TextureViewDescriptor::default());

        // The post bind group reads the screenshot's scene texture, not the
        // window's
        let post_bind_group = self.post.bind_group_for(device, &scene_view);

        // The camera isn't shifted as there's no menu in the screenshot. The
        // aspect ratio is the window's. The dither cells are scale pixels wide
        // The caller writes the window's values again afterwards
        let aspect_ratio = window_width as f32 / window_height as f32;
        let controls = &self.gui.controls;
        let camera_uniform = camera::camera_uniform(
            orbit,
            controls.model_angle,
            controls.light_angle,
            &self.gui.shading,
            aspect_ratio,
            0.0,
        );
        queue.write_buffer(&self.camera_transform.buffer, 0, bytemuck::bytes_of(&camera_uniform));
        queue.write_buffer(
            &self.floor_camera_transform.buffer,
            0,
            bytemuck::bytes_of(&camera_uniform.for_floor(self.mesh.min_y)),
        );
        self.post
            .write_uniform(queue, &self.gui.palette, scale, self.gui.levels);

        // The buffer the image is copied into so the CPU can read it
        let padded_bytes_per_row = padded_bytes_per_row(width);
        let readback_buffer = device.create_buffer(&BufferDescriptor {
            label: Some("Capture readback buffer"),
            size: padded_bytes_per_row as u64 * height as u64,
            usage: BufferUsages::MAP_READ | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Draw the frame, then copy it out
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Capture encoder"),
        });
        self.encode_scene_pass(&mut encoder, &scene_view, &depth_view);
        self.encode_post_pass(&mut encoder, &post_bind_group, &output_view);
        encoder.copy_texture_to_buffer(
            TexelCopyTextureInfo {
                texture: &output_texture,
                mip_level: 0,
                origin: Origin3d::ZERO,
                aspect: TextureAspect::All,
            },
            TexelCopyBufferInfo {
                buffer: &readback_buffer,
                layout: TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bytes_per_row),
                    rows_per_image: Some(height),
                },
            },
            size,
        );
        queue.submit(Some(encoder.finish()));

        // Map the buffer for reading and block until the GPU has finished
        let (sender, receiver) = mpsc::channel();
        readback_buffer.map_async(MapMode::Read, .., move |result| {
            let _ = sender.send(result);
        });
        device.poll(PollType::wait_indefinitely())?;
        receiver.recv()??;

        // Copy the rows without their padding, swapping to RGBA if needed
        let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
        {
            let mapped = readback_buffer.get_mapped_range(..)?;
            for row in mapped.chunks(padded_bytes_per_row as usize).take(height as usize) {
                pixels.extend_from_slice(&row[..width as usize * 4]);
            }
        }
        readback_buffer.unmap();
        if swap_red_blue {
            for pixel in pixels.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
        }

        // Save it. The bytes are in the surface's colour space, which is what
        // a PNG expects when it's sRGB
        std::fs::create_dir_all(CAPTURE_DIR)?;
        let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        let path = PathBuf::from(CAPTURE_DIR).join(format!("reflecdith_{timestamp}.png"));
        image::save_buffer(&path, &pixels, width, height, image::ColorType::Rgba8)?;

        Ok((path, width, height))
    }
}
