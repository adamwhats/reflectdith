# ReflecDith

Rust/wgpu mesh viewer with a palette/dithering post-process and an experimental
ROS 2 robot mode.

## Ordinary viewer

```sh
cargo run
cargo run -- --urdf examples/arm.urdf
```

Drag to orbit, scroll to zoom. Press **Tab** to hide/show the controls;
`--hide-gui` starts with a full-window view. Dithering defaults to three levels. The existing palette, shading, and screenshot
controls also work for robots. Robot mode omits the teapot model controls.

## ROS 2 prototype (Jazzy)

The viewer uses **rclrs directly**, with subscriptions on a background thread.
Joint positions update a URDF link tree; meshes stay on the GPU. Dynamic rclrs
subscriptions use the installed C message type support, so this prototype does
not need generated Rust sensor_msgs/tf2_msgs packages or colcon integration.
`rclrs` is pinned to the 0.7 series, which bundles the interfaces it needs.

### Attach to an olo simulator container

From this repo on the Linux host, with one olo simulator running:

```sh
scripts/ros-viewer
```

The launcher selects the single running `gazebo-web-sim` container. If several
are running, it lists them and asks you to pass `--container NAME`. An explicit
`--image IMAGE` runs without automatic container selection.

The **View → Mesh / Robot** selector switches at runtime and keeps each view’s
camera position. ROS stays connected while viewing meshes, so switching back
uses the latest robot pose. In a ROS-enabled build, selecting Robot also starts
discovery if it was not enabled at launch.

The **Robot** dropdown refreshes automatically and shows robot names, rather
than individual ROS topics. A single available robot is selected automatically; choose a
name when several robots are present. Its description, joint states, and connected
odom/map frame are paired automatically. Selecting another robot reconnects the
viewer without restarting it. The camera follows `base_footprint` in that robot's
namespace, and the floor uses its height.

Advanced CLI arguments can pin nonstandard topic/frame names if needed:

```sh
scripts/ros-viewer --container YOUR_SIMULATOR_CONTAINER -- \
  --description-topic /YOUR_ROBOT/robot_description \
  --joint-topic /YOUR_ROBOT/joint_states
```

The description subscription requests transient-local durability so it can receive
a retained URDF from robot_state_publisher. For diagnostics, `--list-topics` still
lists individual topics and frames in the terminal.

The launcher reuses the selected container's image, network namespace, IPC,
mounted assets, and ROS discovery environment. It sources Jazzy and installed
robot workspaces and finds meshes in their ament prefixes. It mounts this repo
at `/viewer`, passes through the desktop display/GPU, and uses the **host rustup
toolchain** to build inside the ROS image. A cached viewer image layer installs `example_interfaces`, `test_msgs`, and desktop runtime
libraries; no software is installed into the running robot container. Builds use `target/ros2`, run as your user, and use four
compile jobs. The selected image must have a C linker and the ROS/desktop runtime
libraries; the olo simulator images are the intended target. Host and image must
have compatible architecture/glibc for the mounted Rust toolchain.

`ROS_DOMAIN_ID` and `RMW_IMPLEMENTATION` set in your shell override the selected
container's values. The viewer uses a UDP-only Fast DDS profile to avoid shared
memory permission problems with root-run simulator processes. Set
`REFLECDITH_USE_CONTAINER_DDS_PROFILE=1` to retain the container's DDS profile
instead. To troubleshoot discovery without opening a window:

```sh
scripts/ros-viewer -- --list-topics
scripts/ros-viewer -- --probe-ros 5
```

To build before the robot is running (uses `gazebo-web-sim-base:local` by default):

```sh
scripts/ros-viewer --build-only
```

### Geometry and moving bases

You can use an expanded local URDF instead of the description topic. Paths here
are **inside the viewer container**; this checkout is available at `/viewer`.

```sh
scripts/ros-viewer --container YOUR_SIMULATOR_CONTAINER -- \
  --urdf /viewer/examples/arm.urdf \
  --joint-topic /joint_states
```

For `package://` meshes, the viewer searches `AMENT_PREFIX_PATH`. Override a
package's location with repeated `--package NAME=/path/to/package` arguments.
Relative mesh paths resolve against the URDF file's directory, or `--asset-dir`
when receiving the description over ROS. Xacro must be expanded before loading.
Mesh coordinates, URDF scales, and visual/joint origins are preserved; the entire
robot receives one display scale and ROS Z-up to renderer Y-up conversion.

The viewer subscribes to `/tf` and transient-local `/tf_static`, finds the root
transform relative to the selected fixed frame, and applies it to the robot.
Override automatic selection with `--fixed-frame FRAME` if needed. The first root translation establishes the
display origin once, so a distant spawn pose starts in view; subsequent motion
is retained against the stationary floor. Override names with `--tf-topic` and
`--tf-static-topic`. The root TF frame must match the URDF root link name. Missing
TF is shown in the side panel. The camera follows the position and heading of `base_footprint` (automatically
namespaced from the URDF root, e.g. `m20/base_footprint`). Override it with
`--follow-frame FRAME`. Body roll, pitch, and height changes do not drive the
camera. The floor sits at the footprint height, rather than at the lowest point
of the zero-joint pose, and its visible patch stays beneath that frame.

**Lighting → Shadows** toggles directional shadows cast by the robot,
including shadows from one part of the robot onto another. They are on
by default and follow the scene light, including light rotation. Use
`--no-shadows` to start with them disabled. Shadows also appear in exported
screenshots. Floor defaults are brightness **0.5**, fade start **0.6**, and fade
end **2.5**. Shininess ranges from **1** to **64**.

The floor blue noise is anchored to world X/Z coordinates, so it remains fixed
as the robot moves or the camera orbits. Robot mode starts with a stationary light;
**Light speed** enables rotation independently of the robot. Shininess and specular
strength controls remain available below the robot picker; angle fields are omitted.
Specular strength defaults to **1.5**.
Adjust **Floor → Pattern density** in the panel to change its pattern spacing; robot
surfaces retain their Bayer dithering. The camera keeps its orbit and zoom relative to the followed footprint.

### Run without Docker

With a Jazzy installation sourced and a C linker available:

```sh
source /opt/ros/jazzy/setup.bash
cargo run --features ros2 -- --ros --urdf path/to/robot.urdf
```

### Small reproducible demo

In a sourced ROS 2 environment, `python3 examples/publish_arm.py` publishes the
included arm description, two moving joints, and an `odom -> base_link` transform.
Run the viewer on the same ROS domain with `--ros --fixed-frame odom`.
The Python file is only a test publisher; the viewer's ROS connection is Rust.

## Checks and current limits

```sh
cargo test
cargo run -- --urdf examples/arm.urdf --check-robot
cargo run -- --urdf examples/arm.urdf --capture-after 2
```

`--capture-after` saves a PNG in `screenshots/` and exits; it requires a display.
`--list-topics` lists discovered topics and frames after five seconds and exits.
`--probe-ros SECONDS` exits successfully only if it receives valid joint positions.
The side panel reports the description, skipped visuals, and age of joint data.

Supported: fixed, revolute, continuous, prismatic, and mimic joints; STL, OBJ,
Collada/DAE meshes; box, cylinder, and sphere visuals. Updates match joints by
name and retain values omitted from later messages. Unsupported/missing visual
meshes are reported and skipped; a robot with no renderable visuals is rejected.

Prototype limits: monochrome shading (URDF materials/textures ignored), no physics,
no floating/planar/spherical joints, no timestamp interpolation or TF history. It renders the latest received pose; this is
interactive visualization, not a deterministic realtime control loop.

## Performance

Robot uniforms are uploaded in a batch, geometry is shared/indexed, and unchanged
shadow maps are reused. Development builds optimize the rendering and GUI hot
paths while retaining debug symbols. See [performance notes](docs/performance.md)
for measurements and tradeoffs.

Use `--benchmark-frames N` to measure N frames after warmup and exit, for example:

```sh
scripts/ros-viewer --container SIMULATOR -- --benchmark-frames 600
```
