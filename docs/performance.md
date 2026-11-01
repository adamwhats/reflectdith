# Rendering performance

The renderer batches all robot visual uniforms into one upload, precomputes
joint origins and axes, reuses forward-kinematics buffers, and traverses TF trees
without allocating frame-name maps. The GUI refreshes its robot catalog when the
catalog changes, rather than cloning it every frame.

Identical visual geometry shares CPU data and GPU buffers. Meshes use exact
position/normal indexing when it reduces storage. The shadow pass also shares
vertices across normal seams, since depth depends only on position. This keeps
triangle positions, order, winding, and hard surface normals intact.

Shadow maps are reused until the light, floor origin, robot geometry/pose, mesh
rotation, or selected view changes. Moving the camera or editing the palette
can reuse the same map. Floor pixels beyond the fade radius skip noise and
shadow sampling. The finite dither palette is gamma-interpolated on the CPU
when edited, avoiding repeated colour interpolation powers per pixel.

Development builds optimize the application, egui/epaint, and wgpu-core while
retaining debug symbols. The first build of these dependencies takes longer;
subsequent application builds reuse them.

## Measurements

Measured on 2026-11-01, Intel Graphics (ARL), Mesa 25.2.8, Vulkan, a 960×540
window with controls visible, three dither levels, and 2048² shadows enabled.
Both binaries used the same GPU and settings. Each run warmed up for 120 frames
and measured 300 frames. The baseline is `c6763ec` plus the timing instrumentation.

| Scene | Baseline mean frame work | Optimized mean frame work |
| --- | ---: | ---: |
| Static M20 URDF | 36.2 ms | 20.4 ms |
| Live M20 over ROS | 35.5 ms | 14.0 ms |
| Rotating teapot mesh | 17.1 ms | 9.6 ms |

Live M20 CPU preparation fell from 7.20 to 1.35 ms, and CPU command encoding
from 3.90 to 1.79 ms. Its pose changed every measured frame, so the shadow map
was regenerated every frame. The static robot reused one shadow map throughout
the run.

A second live M20 comparison over 600 measured frames recorded 32.9 → 16.7 ms
per frame, with CPU preparation 6.47 → 1.23 ms and command encoding 4.04 →
1.66 ms. Across both runs the live viewer was roughly twice as fast or better.
The FP3 also loaded all 16 visuals and exported a screenshot successfully.

These are CPU preparation + command encoding/submission + GPU completion wait
timings, excluding surface acquisition and presentation. They describe frame
work rather than displayed FPS; compositor pacing and other GPU workloads affect
results. Shadow position indices add about 5 MiB of GPU storage for this M20.

A frozen M20 render at 1920×1080 was pixel-identical to the baseline after the
geometry, uniform, shadow, and palette changes. Tests cover exact index
reconstruction, TF tree traversal, independent visual origins with shared
geometry, shadow invalidation, palette interpolation, and live ROS subscription
switching.

## Reproduce

```sh
# Live robot in a selected simulator:
scripts/ros-viewer --container SIMULATOR -- --benchmark-frames 600

# Static local URDF, without ROS:
cargo run --locked -- --urdf examples/arm.urdf --benchmark-frames 600

# Original mesh view:
cargo run --locked -- --benchmark-frames 600
```

Benchmark mode waits for GPU completion each frame, reports mean/p50/p95 timings,
and exits cleanly. Normal rendering does not perform that GPU wait. Use
`--hide-gui` or `--no-shadows` to measure those configurations separately, and
compare runs with matching scene, pose, window size, and settings.

The selected adapter is printed at startup. `WGPU_ADAPTER_NAME` and
`WGPU_POWER_PREF` are honored and forwarded by the Docker launcher, for example:

```sh
WGPU_ADAPTER_NAME=Intel scripts/ros-viewer --container SIMULATOR -- --benchmark-frames 600
```
