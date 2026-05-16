use crate::config::{save_config, Config, LightConfig};
use rusqlite::Connection;

pub fn find_amaran_db() -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let base = std::path::Path::new(&home).join("Library/Application Support/amaran Desktop");

    if !base.exists() {
        return None;
    }

    std::fs::read_dir(&base)
        .ok()?
        .filter_map(|entry| {
            let path = entry.ok()?.path().join("amaran.db");
            path.exists().then_some(path)
        })
        .next()
        .map(|p| p.to_string_lossy().to_string())
}

fn extract_from_db(db_path: &str) -> anyhow::Result<(String, String, Vec<LightConfig>)> {
    let conn = Connection::open(db_path)?;

    let (net_key, app_key): (String, String) = conn
        .query_row("SELECT net_key, app_key FROM mesh LIMIT 1", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;

    let net_key = net_key.trim().to_uppercase();
    let app_key = app_key.trim().to_uppercase();

    if net_key.is_empty() || app_key.is_empty() {
        anyhow::bail!("could not read net_key/app_key from mesh table");
    }

    let mut stmt = conn.prepare(
        "SELECT mac_address, node_address, name FROM fixtures WHERE node_address > 1 ORDER BY node_address",
    )?;

    let lights: Vec<LightConfig> = stmt
        .query_map([], |row| {
            let mac: String = row.get(0)?;
            let address: i64 = row.get(1)?;
            let name: String = row.get(2)?;
            Ok((mac.trim().to_string(), address as u16, name.trim().to_string()))
        })?
        .filter_map(|r| r.ok())
        .filter(|(_mac, addr, _name)| *addr >= 2)
        .map(|(mac, address, name)| {
            let key = name
                .to_lowercase()
                .chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .take(8)
                .collect::<String>();
            let key = if key.is_empty() {
                format!("light{address}")
            } else {
                key
            };
            LightConfig {
                key,
                name,
                mac,
                address,
            }
        })
        .collect();

    Ok((net_key, app_key, lights))
}

fn ask(prompt: &str) -> String {
    use std::io::{BufRead, Write};
    print!("{prompt}");
    let _ = std::io::stdout().flush();
    let stdin = std::io::stdin();
    let mut line = String::new();
    stdin.lock().read_line(&mut line).unwrap_or_default();
    line.trim().to_string()
}

fn pick_relay_hub(lights: &[LightConfig]) -> anyhow::Result<String> {
    println!("\nwhich light should be used as the BLE relay hub (the one your computer connects to)?");
    for (i, l) in lights.iter().enumerate() {
        println!("  {}. {}  (address {}, MAC {})", i + 1, l.name, l.address, l.mac);
    }
    let ans = ask("enter number [1]: ");
    let idx = ans.parse::<usize>().unwrap_or(1).saturating_sub(1);
    let idx = idx.min(lights.len().saturating_sub(1));
    Ok(lights[idx].mac.clone())
}

fn manual_setup() -> anyhow::Result<Config> {
    println!("\nmanual setup — enter your mesh config values.");
    println!("(find these in ~/Library/Application Support/amaran Desktop/*/amaran.db)\n");

    let net_key = ask("net key (hex, 32 chars): ").to_uppercase();
    let app_key = ask("app key (hex, 32 chars): ").to_uppercase();

    let mut lights: Vec<LightConfig> = Vec::new();
    loop {
        println!("\nlight {}:", lights.len() + 1);
        let name = ask("  name (e.g. Key Light): ");
        let mac = ask("  MAC address (e.g. A4:C1:38:13:41:38): ").to_uppercase();
        let address: u16 = ask("  mesh address (e.g. 2): ").parse().unwrap_or(0);
        let key = name
            .to_lowercase()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .take(8)
            .collect::<String>();
        let key = if key.is_empty() {
            format!("light{address}")
        } else {
            key
        };
        lights.push(LightConfig {
            key,
            name,
            mac,
            address,
        });

        let more = ask("add another light? [y/N]: ");
        if !more.to_lowercase().starts_with('y') {
            break;
        }
    }

    let relay_hub = pick_relay_hub(&lights)?;
    Ok(Config {
        net_key,
        app_key,
        relay_hub,
        lights,
        http: None,
        mqtt: None,
    })
}

pub fn run_setup() -> anyhow::Result<()> {
    println!("═══════════════════════════════════════");
    println!("  amaran light setup");
    println!("═══════════════════════════════════════\n");

    let mut config: Option<Config> = None;

    if let Some(db_path) = find_amaran_db() {
        println!("found amaran database at:\n  {db_path}\n");
        let use_db = ask("auto-import from this database? [Y/n]: ");

        if !use_db.to_lowercase().starts_with('n') {
            match extract_from_db(&db_path) {
                Ok((net_key, app_key, lights)) => {
                    if lights.is_empty() {
                        println!("no lights found in database. falling back to manual setup.");
                    } else {
                        println!("\nfound {} light(s):", lights.len());
                        for l in &lights {
                            println!("  • {}  (address {})", l.name, l.address);
                        }
                        let relay_hub = pick_relay_hub(&lights)?;
                        config = Some(Config {
                            net_key,
                            app_key,
                            relay_hub,
                            lights,
                            http: None,
                            mqtt: None,
                        });
                    }
                }
                Err(e) => {
                    eprintln!("failed to read database: {e}");
                    println!("falling back to manual setup.\n");
                }
            }
        }
    } else {
        println!("amaran desktop database not found (is the app installed?).");
        println!("proceeding with manual setup.\n");
    }

    let mut config = config.unwrap_or_else(|| manual_setup().unwrap_or_else(|e| {
        eprintln!("setup failed: {e}");
        std::process::exit(1);
    }));

    // let user rename light keys
    println!("\nlight shorthand keys (used as CLI targets, e.g. `amaran brightness 50 key`):");
    for light in &mut config.lights {
        let new_key = ask(&format!("  {} → key [{}]: ", light.name, light.key));
        if !new_key.is_empty() {
            light.key = new_key;
        }
    }

    save_config(&config)?;

    println!("\n✓ saved to lights.json");
    let hub_name = config
        .lights
        .iter()
        .find(|l| l.mac.eq_ignore_ascii_case(&config.relay_hub))
        .map(|l| l.name.as_str())
        .unwrap_or(&config.relay_hub);
    println!("  relay hub: {hub_name}");
    println!("  {} light(s) configured", config.lights.len());
    println!("\ntry it:  cargo run -- on");

    Ok(())
}
