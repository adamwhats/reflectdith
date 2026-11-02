mod app;
mod cli;
mod engine;
mod scenes;
#[cfg(target_os = "linux")]
mod wallpaper;

use std::time::{Duration, Instant};

use clap::Parser;
use winit::event_loop::EventLoop;

use scenes::{Scenes, boids::Boids, mesh::MeshScene, robot};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = cli::Cli::parse();
    if let Some(cli::Command::Ctl { command }) = &cli.command {
        use reflecdith_control::ipc;
        if matches!(command, cli::Control::Stop) {
            ipc::stop()?;
            return Ok(());
        }
        let state = match command {
            cli::Control::Stop => unreachable!(),
            cli::Control::Get => ipc::get_state()?,
            cli::Control::Set { json } => ipc::apply_patch(json)?,
            cli::Control::ResetBoids => ipc::reset_boids()?,
            cli::Control::Pause => ipc::apply_patch(r#"{"paused":true}"#)?,
            cli::Control::Resume => ipc::apply_patch(r#"{"paused":false}"#)?,
            cli::Control::Random => ipc::apply_patch(&format!(
                r#"{{"palette":{}}}"#,
                serde_json::to_string(&reflecdith_control::settings::Palette::random_muted())?
            ))?,
            cli::Control::Swap => {
                let mut palette = ipc::get_state()?.settings.palette;
                std::mem::swap(&mut palette.dark, &mut palette.light);
                ipc::apply_patch(&format!(r#"{{"palette":{}}}"#, serde_json::to_string(&palette)?))?
            }
        };
        println!("{}", serde_json::to_string_pretty(&state)?);
        return Ok(());
    }
    if cli.wallpaper && (cli.benchmark_frames.is_some() || cli.capture_after.is_some()) {
        return Err("Wallpaper mode does not support benchmark or timed capture; use the ordinary viewer".into());
    }
    if cli.wallpaper && (cli.robot.ros || cli.robot.urdf.is_some()) {
        return Err("Robot scenes are only available in the ordinary viewer; omit --wallpaper".into());
    }
    let options = &cli.robot;
    let initial_robot = if let Some(path) = &options.urdf {
        let xml = std::fs::read_to_string(path)?;
        let robot = robot::Robot::load(&xml, path.parent().unwrap_or(std::path::Path::new(".")), options)?;
        for warning in &robot.warnings {
            eprintln!("Visual skipped: {warning}");
        }
        eprintln!(
            "Loaded {}: {} visuals, root {}",
            robot.name,
            robot.visuals.len(),
            robot.root
        );
        Some(robot)
    } else {
        None
    };
    if options.check_robot {
        if initial_robot.is_none() {
            return Err("--check-robot requires --urdf".into());
        }
        return Ok(());
    }
    let live = robot::SharedState::default();
    live.lock().unwrap().robot_root = initial_robot.as_ref().map(|r| r.root.clone());
    #[cfg(feature = "ros2")]
    let ros_connection = if options.ros {
        Some(robot::ros::RosConnection::start(options.clone(), live.clone())?)
    } else {
        None
    };
    #[cfg(not(feature = "ros2"))]
    if options.ros {
        return Err("ROS support is disabled; build with --features ros2 inside a sourced ROS 2 environment".into());
    }

    if options.list_topics || options.probe_ros.is_some() {
        let seconds = options.probe_ros.unwrap_or(5);
        if !options.ros {
            return Err("--probe-ros / --list-topics requires --ros".into());
        }
        let deadline = Instant::now() + Duration::from_secs(seconds);
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
            let state = live.lock().unwrap();
            if let Some(error) = &state.error {
                return Err(error.clone().into());
            }
        }
        let state = live.lock().unwrap();
        eprintln!(
            "ROS probe: {} named joints, description={}, TF edges={}",
            state.positions.len(),
            state.description.is_some(),
            state.tf.len()
        );
        if options.list_topics {
            for (kind, names) in [
                ("Descriptions (std_msgs/msg/String)", &state.topics.descriptions),
                ("Joint states", &state.topics.joints),
                ("TF topics", &state.topics.transforms),
                ("TF frames", &state.topics.frames),
            ] {
                println!("{kind}:");
                for name in names {
                    println!("  {name}");
                }
            }
            println!("Selected: {:?}", state.active);
            return Ok(());
        }
        if state.last_joint.is_none() {
            return Err("No valid joint positions received during probe".into());
        }
        return Ok(());
    }

    // Every scene the View row offers, in order. Each is built the first time
    // it is shown
    let mut scenes = Scenes::new();
    let mesh = scenes.register("Mesh", |ctx| MeshScene::new(ctx));
    scenes.register("Boids", |ctx| Boids::new(ctx.device, 64));
    let startup = robot::Startup {
        options: options.clone(),
        live,
        robot: initial_robot,
        #[cfg(feature = "ros2")]
        connection: ros_connection,
    };
    let robot = scenes.register("Robot", move |ctx| robot::RobotScene::new(ctx, startup));
    // Start on the robot when one was given on the command line
    let initial_scene = if options.ros || options.urdf.is_some() {
        robot
    } else {
        mesh
    };

    if cli.wallpaper || cli.list_outputs {
        #[cfg(target_os = "linux")]
        return wallpaper::run(cli, scenes);
        #[cfg(not(target_os = "linux"))]
        return Err("Wallpaper mode requires Linux and Wayland".into());
    }
    let event_loop = EventLoop::new().expect("Failed to create event loop");
    // Create the application handler, and run the event loop with it
    let mut app = app::App::new(cli, scenes, initial_scene);
    event_loop.run_app(&mut app).expect("Failed to run event loop");
    eprintln!("Viewer shut down cleanly");
    Ok(())
}
