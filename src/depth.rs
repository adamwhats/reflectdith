use wgpu::{
    Device, Extent3d, TextureDescriptor, TextureDimension, TextureFormat, TextureUsages, TextureView,
    TextureViewDescriptor,
};

pub const DEPTH_FORMAT: TextureFormat = TextureFormat::Depth32Float;

pub fn create_depth_view(device: &Device, width: u32, height: u32) -> TextureView {
    // Create a depth texture view on the device of a given width/height
    let depth_texture = device.create_texture(&TextureDescriptor {
        label: Some("Depth texture"),
        size: Extent3d {
            width: width,
            height: height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: TextureDimension::D2,
        format: DEPTH_FORMAT,
        usage: TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    depth_texture.create_view(&TextureViewDescriptor::default())
}
