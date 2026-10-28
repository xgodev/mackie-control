//! `mackie`: run a bridge, list surfaces, or watch what a surface sends.

use std::process::ExitCode;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};

use mackie_control::describe::{describe, main_driver, Drivers};
use mackie_control::run::{find_port, run, CoreMidi, Deps, Midi};
use mackie_control::{defaults, drivers, profile, protocol, service, surfaces};

#[derive(Parser)]
#[command(name = "mackie", version, about = "A Mackie Control surface driving audio gear through a YAML profile")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run a surface against a rig profile.
    Bridge {
        profile: String,
        /// Exact MIDI port, when the surface's hint is not enough.
        #[arg(long)]
        port: Option<String>,
    },
    /// Control surfaces this program knows.
    Surfaces,
    /// MIDI ports visible right now.
    Ports,
    /// What each bank is and which scenes it can load.
    Devices { profile: String },
    /// Print decoded messages from a surface (mapping a new one).
    Watch {
        #[arg(default_value_t = 30)]
        seconds: u64,
        #[arg(long)]
        surface: Option<String>,
        #[arg(long)]
        port: Option<String>,
    },
    /// Run the bridge at login and restart it if it exits (macOS).
    Service {
        #[arg(value_parser = ["install", "uninstall", "restart", "status"])]
        action: String,
        profile: Option<String>,
    },
}

fn surfaces_list() {
    for (key, s) in surfaces::SURFACES.iter() {
        println!("{key}: {} -- {} faders, {} encoders, ports {:?}", s.name, s.faders, s.encoders, s.port_hints);
        if !s.notes.is_empty() {
            println!("  {}", s.notes);
        }
    }
}

fn ports() -> Result<(), String> {
    let mut midi = CoreMidi;
    println!("inputs:");
    for n in midi.input_names()? {
        println!("  {n}");
    }
    println!("outputs:");
    for n in midi.output_names()? {
        println!("  {n}");
    }
    Ok(())
}

/// Print what the surface sends, decoded -- how every mapping here was found.
fn watch(seconds: u64, surface: Option<&str>, port: Option<String>) -> Result<(), String> {
    let surface = surfaces::get(surface)?;
    let mut midi = CoreMidi;
    let port = match port {
        Some(p) => p,
        None => {
            let hints: Vec<String> = surface.port_hints.iter().map(|h| h.to_string()).collect();
            find_port(&hints, &midi.input_names()?)?
        }
    };
    println!("listening on {port} for {seconds}s (Ctrl-C to stop)");
    let mut inp = midi.open_input(&port)?;
    let end = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < end {
        for msg in inp.pending() {
            if let Some(event) = protocol::decode(&msg) {
                println!("  {event:?}");
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

/// What each bank is: its device, its faders and the scenes it can load.
fn devices(path: &str) -> Result<(), String> {
    let p = defaults::expand(profile::load_profile(path)?)?;
    let mut built: Drivers = Drivers::new();
    for bank in &p.banks {
        if let Some(name) = bank.driver.clone().or_else(|| main_driver(bank)) {
            built.entry(name.clone()).or_insert_with(|| drivers::build(&name));
        }
    }
    for line in describe(&p, &mut built) {
        println!("{line}");
    }
    Ok(())
}

fn service_cmd(action: &str, profile: Option<&str>) -> Result<(), String> {
    let home = service::home();
    let mut run = service::launchctl;
    match action {
        "install" => {
            let profile = profile.ok_or("service install needs PROFILE.yaml")?;
            let binary = std::env::current_exe().map_err(|e| e.to_string())?;
            let dest = service::install(profile, &mut run, &home, &binary)?;
            println!("installed {}; log in {}", dest.display(), service::log_path(&home).display());
        }
        "uninstall" => println!("removed {}", service::uninstall(&mut run, &home).display()),
        "restart" => {
            service::restart(&mut run)?;
            println!("restarted");
        }
        _ => println!("{}", service::status(&mut run, &home)),
    }
    Ok(())
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.cmd {
        Cmd::Bridge { profile, port } => run(&profile, port.as_deref(), Deps::real()),
        Cmd::Surfaces => {
            surfaces_list();
            Ok(())
        }
        Cmd::Ports => ports(),
        Cmd::Devices { profile } => devices(&profile),
        Cmd::Watch { seconds, surface, port } => watch(seconds, surface.as_deref(), port),
        Cmd::Service { action, profile } => service_cmd(&action, profile.as_deref()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
