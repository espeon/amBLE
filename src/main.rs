use amble::config;
use amble::controller;
use amble::daemon;

use clap::{Parser, Subcommand};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tracing::info;

#[derive(Parser)]
#[command(name = "amaran", about = "Control Amaran/Aputure lights via BLE")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    #[arg(long, hide = true)]
    daemon: bool,
}

#[derive(Subcommand)]
enum Commands {
    On {
        light: Option<String>,
    },
    Off {
        light: Option<String>,
    },
    Brightness {
        value: u8,
        light: Option<String>,
    },
    Cct {
        brightness: u8,
        kelvin: u16,
        #[arg(default_value = "0")]
        gm: i8,
        light: Option<String>,
    },
    Hsi {
        brightness: u8,
        hue: u16,
        saturation: u16,
        light: Option<String>,
    },
    Rgb {
        r: u8,
        g: u8,
        b: u8,
        #[arg(default_value = "100")]
        brightness: u8,
        light: Option<String>,
    },
    // untested — Gel, Xy, DimCurve removed until payload confirmed on hardware
    Status {
        light: Option<String>,
    },
    Battery {
        light: Option<String>,
    },
    Query {
        #[arg(default_value = "0a")]
        cmd_type: String,
        light: Option<String>,
    },
    Start,
    Stop,
    Lights,
    Scan,
    /// Import from Amaran Desktop database or set up manually
    Setup,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "amaran_ble=info".into()),
        )
        .init();

    let cli = Cli::parse();

    if cli.daemon {
        let config_path = std::env::current_dir()?.join("lights.json");
        let config = config::load_config(&config_path)?;
        return daemon::run_daemon(config).await;
    }

    match cli.command {
        None => run_repl().await,
        Some(cmd) => match cmd {
            Commands::On { light } => run_command("on", &[], light).await,
            Commands::Off { light } => run_command("off", &[], light).await,
            Commands::Brightness { value, light } => {
                run_command("brightness", &[value.to_string()], light).await
            }
            Commands::Cct {
                brightness,
                kelvin,
                gm,
                light,
            } => {
                run_command(
                    "cct",
                    &[brightness.to_string(), kelvin.to_string(), gm.to_string()],
                    light,
                )
                .await
            }
            Commands::Hsi {
                brightness,
                hue,
                saturation,
                light,
            } => {
                run_command(
                    "hsi",
                    &[
                        brightness.to_string(),
                        hue.to_string(),
                        saturation.to_string(),
                    ],
                    light,
                )
                .await
            }
            Commands::Rgb {
                r, g, b, brightness, light,
            } => {
                run_command(
                    "rgb",
                    &[r.to_string(), g.to_string(), b.to_string(), brightness.to_string()],
                    light,
                )
                .await
            }
            // untested — Gel, Xy match arms removed
            Commands::Status { light } => run_command("status", &[], light).await,
            Commands::Battery { light } => run_command("battery", &[], light).await,
            Commands::Query {
                cmd_type, light,
            } => {
                run_command("query", &[cmd_type.clone()], light).await
            }
            Commands::Start => start_daemon().await,
            Commands::Stop => stop_daemon().await,
            Commands::Lights => list_lights().await,
            Commands::Scan => scan_devices().await,
            Commands::Setup => {
                amble::setup::run_setup()?;
                Ok(())
            }
        },
    }
}

async fn run_command(cmd: &str, args: &[String], light: Option<String>) -> anyhow::Result<()> {
    let config_path = std::env::current_dir()?.join("lights.json");
    let config = config::load_config(&config_path)?;

    if std::path::Path::new(&daemon::socket_path()).exists() {
        let req = serde_json::json!({"cmd": cmd, "args": args, "light": light});
        match send_to_daemon(&req).await {
            Ok(resp) => {
                if let Some(err) = resp.get("error") {
                    eprintln!("error: {err}");
                    std::process::exit(1);
                }
                return Ok(());
            }
            Err(e) => {
                info!("daemon unavailable ({e}), falling back to direct connection");
            }
        }
    }

    let mut ctrl = controller::MeshController::new(config).await?;
    if !ctrl.connect().await? {
        anyhow::bail!("could not connect to lights");
    }
    ctrl.wait_for_beacon(4000).await;
    ctrl.setup_proxy_filter().await?;

    let req = serde_json::json!({"cmd": cmd, "args": args, "light": light});
    let result = daemon::execute_command_json(&mut ctrl, &req).await?;
    if result != "ok" {
        println!("{result}");
    }

    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    ctrl.disconnect().await?;
    Ok(())
}

async fn run_repl() -> anyhow::Result<()> {
    let config_path = std::env::current_dir()?.join("lights.json");
    if !config_path.exists() {
        eprintln!("no lights.json found. run setup: npm run setup");
        std::process::exit(1);
    }

    if std::path::Path::new(&daemon::socket_path()).exists() {
        println!("daemon running — commands are instant. type 'exit' or ctrl+c to quit.");
        repl_via_daemon().await?;
    } else {
        let config = config::load_config(&config_path)?;
        println!("connecting to lights (start daemon for instant commands)...");
        let mut ctrl = controller::MeshController::new(config).await?;
        if !ctrl.connect().await? {
            anyhow::bail!("could not connect");
        }
        ctrl.wait_for_beacon(4000).await;
        ctrl.setup_proxy_filter().await?;
        println!("connected. type 'exit' or ctrl+c to quit.\n");
        repl_direct(ctrl).await?;
    }
    Ok(())
}

async fn repl_via_daemon() -> anyhow::Result<()> {
    let stdin = tokio::io::stdin();
    let mut reader = tokio::io::BufReader::new(stdin);
    let mut line = String::new();

    loop {
        print!("a> ");
        let _ = std::io::Write::flush(&mut std::io::stdout());
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed == "exit" || trimmed == "quit" {
            break;
        }
        if trimmed == "help" {
            println!("{}", daemon::HELP_TEXT);
            continue;
        }

        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        let (cmd, args, light) = parse_line(&parts);

        let req = serde_json::json!({"cmd": cmd, "args": args, "light": light});
        match send_to_daemon(&req).await {
            Ok(resp) => {
                if let Some(err) = resp.get("error") {
                    eprintln!("error: {err}");
                } else if let Some(result) = resp.get("result") {
                    let s = match result {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    if s != "ok" {
                        println!("{s}");
                    }
                }
            }
            Err(e) => eprintln!("error: {e}"),
        }
    }
    Ok(())
}

fn parse_line(parts: &[&str]) -> (String, Vec<String>, Option<String>) {
    let config_path = std::env::current_dir()
        .ok()
        .map(|cwd| cwd.join("lights.json"))
        .unwrap_or_default();
    let known_keys: Vec<String> = if config_path.exists() {
        config::load_config(&config_path)
            .map(|c| c.lights.iter().map(|l| l.key.clone()).collect())
            .unwrap_or_default()
    } else {
        vec![]
    };

    let mut light: Option<String> = None;
    let mut cmd_parts = parts.to_vec();

    if let Some(last) = cmd_parts.last() {
        if known_keys.contains(&last.to_string()) || *last == "all" {
            light = Some(last.to_string());
            cmd_parts.pop();
        }
    }

    let cmd = cmd_parts.first().map(|s| s.to_string()).unwrap_or_default();
    let args: Vec<String> = cmd_parts.iter().skip(1).map(|s| s.to_string()).collect();

    (cmd, args, light)
}

async fn repl_direct(mut ctrl: controller::MeshController) -> anyhow::Result<()> {
    let stdin = tokio::io::stdin();
    let mut reader = tokio::io::BufReader::new(stdin);
    let mut line = String::new();

    loop {
        print!("a> ");
        let _ = std::io::Write::flush(&mut std::io::stdout());
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed == "exit" || trimmed == "quit" {
            break;
        }
        if trimmed == "help" {
            println!("{}", daemon::HELP_TEXT);
            continue;
        }

        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        let (cmd, args, light) = parse_line(&parts);
        let req = serde_json::json!({"cmd": cmd, "args": args, "light": light});

        match daemon::execute_command_json(&mut ctrl, &req).await {
            Ok(msg) if msg != "ok" => println!("{msg}"),
            Err(e) => eprintln!("error: {e}"),
            _ => {}
        }
    }

    ctrl.disconnect().await?;
    Ok(())
}

async fn send_to_daemon(req: &serde_json::Value) -> anyhow::Result<serde_json::Value> {
    let sock_path = daemon::socket_path();
    let stream = tokio::net::UnixStream::connect(&sock_path).await?;
    let (reader, mut writer) = tokio::io::split(stream);
    let mut reader = tokio::io::BufReader::new(reader);

    let payload = format!("{}\n", serde_json::to_string(req)?);
    writer.write_all(payload.as_bytes()).await?;

    let mut line = String::new();
    reader.read_line(&mut line).await?;

    Ok(serde_json::from_str(line.trim())?)
}

async fn start_daemon() -> anyhow::Result<()> {
    if std::path::Path::new(&daemon::socket_path()).exists() {
        println!("daemon already running. stop it first: amaran stop");
        return Ok(());
    }

    let exe = std::env::current_exe()?;
    let child = std::process::Command::new(exe)
        .arg("--daemon")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;

    println!("daemon starting (PID: {})...", child.id());
    println!("run 'amaran stop' to stop it.");
    Ok(())
}

async fn stop_daemon() -> anyhow::Result<()> {
    let sock_path = daemon::socket_path();
    let pid_path = daemon::pid_path();

    if !std::path::Path::new(&sock_path).exists() {
        println!("daemon is not running.");
        return Ok(());
    }

    let req = serde_json::json!({"cmd": "stop", "args": []});
    match send_to_daemon(&req).await {
        Ok(_) => println!("daemon stopped."),
        Err(_) => {
            let _ = std::fs::remove_file(&sock_path);
            let _ = std::fs::remove_file(&pid_path);
            println!("daemon stopped.");
        }
    }
    Ok(())
}

async fn list_lights() -> anyhow::Result<()> {
    let config_path = std::env::current_dir()?.join("lights.json");
    let config = config::load_config(&config_path)?;
    println!("configured lights:");
    for light in &config.lights {
        let hub = light.mac.to_uppercase() == config.relay_hub.to_uppercase();
        println!(
            "  {:<8} {}  (addr {}, MAC {}){}",
            light.key,
            light.name,
            light.address,
            light.mac,
            if hub { "  ← relay hub" } else { "" }
        );
    }
    Ok(())
}

async fn scan_devices() -> anyhow::Result<()> {
    use btleplug::api::{Central, Manager as _, Peripheral as _, ScanFilter};
    use btleplug::platform::Manager;

    let manager = Manager::new().await?;
    let adapters = manager.adapters().await?;
    let adapter = adapters
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("no bluetooth adapter"))?;

    adapter.start_scan(ScanFilter::default()).await?;
    println!("scanning for 10 seconds...");
    tokio::time::sleep(std::time::Duration::from_secs(10)).await;

    let periphs = adapter.peripherals().await.unwrap_or_default();
    adapter.stop_scan().await?;

    println!("\ndevices found:");
    for p in &periphs {
        let props = p.properties().await.unwrap_or_default();
        if let Some(props) = props {
            let name = props.local_name.as_deref().unwrap_or("(unknown)");
            let addr = p.id().to_string();
            let services: Vec<String> = props
                .services
                .iter()
                .map(|s: &uuid::Uuid| {
                    s.as_hyphenated()
                        .to_string()
                        .split('-')
                        .next()
                        .unwrap_or("")
                        .to_string()
                })
                .collect();
            let is_amaran = name.to_lowercase().contains("amaran")
                || name.to_lowercase().contains("aputure")
                || name.to_lowercase().contains("slck");
            println!(
                "  {}{:<30} {}  rssi={}  svc={}",
                if is_amaran { "* " } else { "  " },
                name,
                addr,
                props.rssi.unwrap_or(0),
                services.join(",")
            );
        }
    }

    Ok(())
}
