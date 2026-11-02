# Live wallpaper and COSMIC controls

ReflecDith can render directly into a Wayland background layer. The wallpaper is
an independent process; the COSMIC applet and settings window control it live
over the session D-Bus. Closing either controller leaves the wallpaper running.

## Run from the checkout

```sh
cargo run -- --list-outputs
cargo run -- --wallpaper --fps 30
```

By default, all connected monitors are used. Boids span the logical desktop
layout as one shared flock. Mesh and Robot are available in the ordinary viewer.
Use `--output DP-1` to restrict the wallpaper to one output. Wallpaper mode
requires Linux, a session bus, a GPU supported by wgpu, and
a compositor supporting `wlr-layer-shell` (including COSMIC). It never takes
keyboard focus, consumes pointer input, or reserves space for panels.

In a second terminal:

```sh
cargo run -- ctl get
cargo run -- ctl set '{"selected_scene":1,"scene":{"boids":{"trail_length":1.5}}}'
cargo run -- ctl set '{"levels":4,"shading":{"shadow_strength":0.5}}'
cargo run -- ctl random
cargo run -- ctl swap
cargo run -- ctl pause
cargo run -- ctl resume
cargo run -- ctl stop
```

Boids is currently the only wallpaper scene (stable ID 1). Mesh and Robot stay
in the ordinary viewer, including its ROS container launcher. Saved viewer-only
scene selections migrate to the first wallpaper scene; control requests for
unsupported scenes are rejected atomically. `--wallpaper` cannot be combined
with `--ros` or `--urdf`.

Patches update only named fields. Invalid ranges and unknown field names are
rejected atomically. Changes reach the renderer between frames, including while
paused (a paused wallpaper redraws settings changes without advancing time).

Camera angles are degrees; distance uses scene units:

```sh
cargo run -- ctl set '{"camera":{"yaw":40,"pitch":25,"distance":8}}'
cargo run -- ctl set '{"camera":null}' # restore the active scene’s own orbit
```

## Monitor layout and slow motion

Boids render once into one shared desktop canvas. Each monitor shows the crop
corresponding to its logical position and size in COSMIC Display Settings.
Rearranging, rotating, attaching, or removing an output updates the views without
restarting or respawning the flock. The camera keeps its original position and
bird bodies keep their original size. The flight box fits the combined desktop's
aspect ratio: side-by-side screens get a wide cuboid, while stacked screens get
a taller volume. The automatic boids wallpaper uses orthographic projection,
keeping bird size and the left/right walls consistent at every depth. Wall
steering predicts a turning arc from each bird's speed, eases in gradually, and
preserves speed through the turn. Faster birds start turning earlier, while
still reaching the outer parts of the desktop. All eight corners, including the
body radius, fit inside a small screen-edge margin. Width expands to use the available horizontal view instead
of moving the camera away. Positions and trail history remap into the new volume,
with steering speed preserved. Soft wall steering has a hard
containment limit as a backstop. Explicit camera settings keep your chosen
framing, including any intentional close-up. The service stays alive with
no monitors and resumes presenting when an output returns.

The applet's **Simulation speed** slider changes boid simulation time independently
of the render FPS: 1× is real time, 0.25× is quarter speed, and 0× freezes the flock
while appearance and camera controls still work. Trail lengths are measured in
simulation seconds, so slowing time preserves their shape. Light rotation keeps
its separate speed control. The same boid speed slider is available in the normal
viewer's controls.

```sh
cargo run -- ctl set '{"scene":{"boids":{"time_scale":0.25}}}'
cargo run -- ctl set '{"scene":{"boids":{"time_scale":1}}}'
```

The renderer substeps boid motion to keep steering stable at lower FPS and higher
simulation speeds. The 0–2× setting persists with the other boid settings; older
settings files default to 1×.

## Build the COSMIC controls

The applet is a separate Cargo project so ordinary viewer builds do not compile
libcosmic. Its toolkit revision and dependency lockfile are pinned.

```sh
cargo build --locked --manifest-path applet/Cargo.toml
REFLECDITH_BIN="$PWD/target/debug/reflecdith" \
  applet/target/debug/reflecdith-applet --settings
```

The same binary runs as a COSMIC panel applet when launched without `--settings`.
The popup includes scene selection, Dark/Light colour swatches with COSMIC
colour pickers (colour plane, hue, hex/RGB input, live preview), random/swap,
dithering, pause/resume, stop, FPS, boid trails, lighting, floor,
simulation speed, and advanced flocking settings. Trails and **Reset boids** are
grouped with the flocking controls under **More settings → Boids**. Reset restores
boid defaults and respawns the flock with empty trails, preserving the fitted
volume, palette, and other wallpaper settings. Camera controls are available
through the CLI rather than the applet. It shows the current
outputs. Leave the output field blank for all monitors, or enter one output name
to restrict the wallpaper. Stop and start the wallpaper to change that selection.

Bayer size is selected in whole pixels (1–8 px) in both interfaces. The renderer
snaps legacy `bayer_density` values to the nearest reciprocal integer cell size.
This prevents uneven cell widths and large moiré grids from fractional sampling.
Screenshot scaling enlarges each complete cell by the capture scale.

Slider edits are coalesced while a request is in flight. State-change signals
keep open controllers updated, with a one-second status/reconnect fallback.
Controllers do no bus polling while their panel popup is closed.

## Install for your user

```sh
scripts/install-wallpaper
```

This builds release binaries and installs them under `~/.local`, along with
model assets and desktop entries. It does not start a wallpaper or change your
panel configuration. Pass a different prefix as the first argument if needed.
The build requires the native development libraries used by libcosmic; on
Pop!_OS the toolkit's build dependencies should be installed first. Refer to
[libcosmic's build instructions](https://github.com/pop-os/libcosmic#building).

If COSMIC Settings was already open during installation, close and reopen it
to refresh the available applet list. Open COSMIC panel settings, choose
**Configure panel applets → Add applet**, and add **ReflecDith**. Alternatively,
launch **ReflecDith Settings** from the application launcher. Press **Start
wallpaper** in either controller. The installed applet finds the wallpaper binary
beside itself, even if `~/.local/bin` is absent from your PATH.

The renderer finds models from `REFLECDITH_MODEL_DIR`, a `models` directory in the
working directory, the installed `share/reflecdith/models`, or the checkout's
models during development. `REFLECDITH_BIN` overrides the binary the applet starts.

## Saved settings and lifecycle

Settings are loaded from `$XDG_CONFIG_HOME/reflecdith/settings.json`, falling back
to `~/.config/reflecdith/settings.json`. Updates are saved atomically after 500 ms
without further edits, and flushed on Stop, SIGINT, or SIGTERM. Monitor removal
keeps the service and simulation running for the remaining outputs.
Light angle and simulation positions are runtime state, not saved settings.
Command-line `--fps` and `--no-shadows` override saved
values for that run; a later settings edit saves the resulting configuration.
The ordinary viewer starts with its existing defaults and does not overwrite the
wallpaper settings.

Only one renderer owns `io.github.reflecdith.Wallpaper` per session. A second
instance fails instead of replacing or competing with it. Its interface is
`io.github.reflecdith.Wallpaper1` at `/io/github/reflecdith/Wallpaper`:

- `GetState() -> JSON`: revision, settings, output names, logical output geometry,
  whether the current scene spans, and the optional single-output restriction.
- `ApplyPatch(JSON) -> JSON`: validate/apply and return accepted state.
- `StateChanged(JSON)`: notification to controllers.
- `Stop()`: request a clean exit and flush pending settings.

Pause/resume are patches to `paused`. This is a local session-bus API; there is
no network listener.

## Current limits

- Spanning uses the rectangle around all active outputs. Boids can temporarily
  disappear into gaps in an irregular monitor layout; they do not avoid holes.
- Monitors present independently. A slow or hidden output can skip frames without
  holding up the flock or other outputs; this is not hardware frame synchronization.
- Very large layouts are uniformly reduced to fit the GPU texture-size limit.
- Integer buffer scaling is supported. Fractional scaling currently relies on
  compositor resampling rather than a fractional-scale/viewport implementation.
- Manual pause and compositor frame callbacks limit work; there is no explicit
  fullscreen-window detection or automatic battery policy.
- Settings persist, but named presets and automatic session startup are not yet
  provided. Add the installed wallpaper command to COSMIC Startup Applications
  if you want it launched at login.
- Mesh and Robot (including ROS discovery) are viewer-only. The applet lists
  only scenes explicitly enabled for wallpaper mode.

The background uses the Background layer. Other wallpaper programs can occupy
that same layer, so their startup/order may affect which background is visible;
check for competing wallpaper processes if ReflecDith runs but is hidden.

## Verification

```sh
cargo test --workspace
# Isolated bus, no desktop or user settings touched:
dbus-run-session -- cargo test -p reflecdith-control session_bus_roundtrip -- --ignored
cargo test --manifest-path applet/Cargo.toml
# GPU rendering, monitor-crop, and slow-motion regressions:
cargo test --bin reflecdith -- --ignored
```

## Scene integration

The Wayland host (`src/wallpaper.rs`), desktop geometry (`src/wallpaper/layout.rs`),
and per-output presentation (`src/wallpaper/present.rs`) are separate from the
winit viewer. Both use the same rendering engine and `Scene` interface; scenes
never manage Wayland surfaces or monitor connections.

Each scene declares `Scene::wallpaper_mode()` using `WallpaperMode::{None, Single, Span}`.
`None` (the default) keeps a scene viewer-only. `Single` repeats one view on each
monitor; `Span` uses a shared desktop-sized view with a crop for each monitor.
These static capabilities are collected at registration,
without constructing scenes or starting ROS. The backend publishes that catalog
over IPC; the applet offers it automatically, and control patches are checked
against it. `Scene::fit_wallpaper` handles scene-specific framing and bounds.

To add a scene, implement `Scene`, choose a wallpaper mode, and register its factory in `main`.
Only scene-specific settings and applet controls need further integration. This
is reusable within this renderer, rather than a standalone wallpaper library:
settings and control actions still belong to ReflecDith, and the platform host
requires Wayland's layer-shell protocol.
