use crate::config::{Config, HttpConfig};
use crate::controller::MeshController;
use rumqttc::{AsyncClient, MqttOptions, QoS};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;
use tracing::{error, info, warn};

pub fn socket_path() -> String {
    let tmp = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".into());
    format!("{tmp}amaran-light.sock")
}

pub fn pid_path() -> String {
    let tmp = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".into());
    format!("{tmp}amaran-light.pid")
}

pub async fn run_daemon(config: Config) -> anyhow::Result<()> {
    let mut ctrl = MeshController::new(config.clone()).await?;

    info!("connecting to lights...");
    if !ctrl.connect().await? {
        anyhow::bail!("failed to connect");
    }
    ctrl.wait_for_beacon(4000).await;
    ctrl.setup_proxy_filter().await?;

    let sock_path = socket_path();
    let pid_path = pid_path();

    let _ = std::fs::remove_file(&sock_path);
    std::fs::write(&pid_path, std::process::id().to_string())?;

    info!("ready — listening on {sock_path}");

    let ctrl = Arc::new(Mutex::new(ctrl));
    let http_cfg = config.http.unwrap_or(HttpConfig {
        port: 2708,
        host: "0.0.0.0".into(),
        api_key: None,
    });

    // MQTT
    if let Some(ref mqtt_cfg) = config.mqtt {
        let port: u16 = mqtt_cfg
            .broker
            .split(':')
            .nth(2)
            .and_then(|s| s.parse().ok())
            .unwrap_or(1883);
        let host = mqtt_cfg
            .broker
            .strip_prefix("mqtt://")
            .unwrap_or(&mqtt_cfg.broker)
            .split(':')
            .next()
            .unwrap_or("localhost");

        let mut mqtt_opts = MqttOptions::new("amaran-daemon", host, port);
        mqtt_opts.set_keep_alive(std::time::Duration::from_secs(30));
        if let Some(ref u) = mqtt_cfg.username {
            mqtt_opts.set_credentials(u, mqtt_cfg.password.as_deref().unwrap_or(""));
        }
        let (client, mut eventloop) = AsyncClient::new(mqtt_opts, 100);
        let topic_prefix = mqtt_cfg.topic_prefix.clone();
        let discovery_prefix = mqtt_cfg.discovery_prefix.clone();

        client
            .publish(
                format!("{topic_prefix}/status"),
                QoS::AtLeastOnce,
                true,
                b"online",
            )
            .await?;

        for light in &config.lights {
            let kelvin_to_mireds = |k: f64| -> u16 { (1000000.0 / k).round() as u16 };
            let discovery = serde_json::json!({
                "name": light.name,
                "unique_id": format!("amaran_{}", light.key),
                "schema": "json",
                "command_topic": format!("{topic_prefix}/{}/set", light.key),
                "state_topic": format!("{topic_prefix}/{}/state", light.key),
                "availability_topic": format!("{topic_prefix}/status"),
                "supported_color_modes": ["color_temp", "hs"],
                "brightness_scale": 255,
                "min_mireds": kelvin_to_mireds(7500.0),
                "max_mireds": kelvin_to_mireds(2500.0),
                "device": {
                    "identifiers": [format!("amaran_{}", light.key)],
                    "name": &light.name,
                    "model": "Amaran Light",
                    "manufacturer": "Aputure",
                },
            });
            client
                .publish(
                    format!("{discovery_prefix}/light/amaran_{}/config", light.key),
                    QoS::AtLeastOnce,
                    true,
                    serde_json::to_string(&discovery)?.as_bytes(),
                )
                .await?;
        }

        info!("MQTT connected — discovery published");

        let ctrl_mqtt = ctrl.clone();
        let tp = topic_prefix.clone();
        tokio::spawn(async move {
            loop {
                match eventloop.poll().await {
                    Ok(rumqttc::Event::Incoming(rumqttc::Packet::Publish(p))) => {
                        let payload: serde_json::Value =
                            match serde_json::from_slice(&p.payload) {
                                Ok(v) => v,
                                Err(_) => continue,
                            };
                        let light_key = p
                            .topic
                            .strip_prefix(&format!("{tp}/"))
                            .and_then(|s| s.strip_suffix("/set"))
                            .unwrap_or("");
                        let cmd = parse_ha_command(&payload, light_key);
                        if let Err(e) =
                            execute_command_json(&mut *ctrl_mqtt.lock().await, &cmd).await
                        {
                            warn!("MQTT command failed: {e}");
                        }
                    }
                    Ok(_) => {}
                    Err(e) => {
                        warn!("MQTT error: {e}");
                        break;
                    }
                }
            }
        });
    }

    // HTTP server (sync, separate thread)
    let addr = format!("{}:{}", http_cfg.host, http_cfg.port);
    let ctrl_http = ctrl.clone();
    let lights_json = serde_json::to_value(&config.lights)?;
    let api_key = http_cfg.api_key.clone();
    std::thread::spawn(move || {
        use tiny_http::{Header, Method, Response, StatusCode};

        let server = match tiny_http::Server::http(&addr) {
            Ok(s) => s,
            Err(e) => {
                error!("HTTP server failed: {e}");
                return;
            }
        };
        info!("HTTP API → http://{}", addr);

        for mut request in server.incoming_requests() {
            fn cors(r: Response<&[u8]>) -> Response<&[u8]> {
                r.with_header(
                    Header::from_bytes(
                        b"Access-Control-Allow-Origin".as_slice(),
                        b"*".as_slice(),
                    )
                    .unwrap(),
                )
                .with_header(
                    Header::from_bytes(
                        b"Access-Control-Allow-Methods".as_slice(),
                        b"GET, POST, OPTIONS".as_slice(),
                    )
                    .unwrap(),
                )
                .with_header(
                    Header::from_bytes(
                        b"Access-Control-Allow-Headers".as_slice(),
                        b"Content-Type, Authorization".as_slice(),
                    )
                    .unwrap(),
                )
            }

            let json = |status: u16, body: String| -> Response<&[u8]> {
                let body = body.into_bytes().leak();
                cors(
                    Response::new(
                        StatusCode(status),
                        vec![Header::from_bytes(
                            b"Content-Type".as_slice(),
                            b"application/json".as_slice(),
                        )
                        .unwrap()],
                        body,
                        Some(body.len()),
                        None,
                    ),
                )
            };

            if request.method() == &Method::Options {
                let _ = request.respond(json(204, String::new()));
                continue;
            }

            if let Some(ref key) = api_key {
                let auth = request
                    .headers()
                    .iter()
                    .find(|h| h.field.equiv("Authorization"))
                    .map(|h| h.value.as_str())
                    .unwrap_or("");
                if auth != format!("Bearer {key}") {
                    let _ = request.respond(json(
                        401,
                        serde_json::json!({"ok": false, "error": "unauthorized"}).to_string(),
                    ));
                    continue;
                }
            }

            let url = request.url().to_string();
            let method = request.method().clone();

            if method == Method::Get && (url == "/" || url == "/lights") {
                let body = serde_json::json!({"ok": true, "lights": lights_json, "daemon": true});
                let _ = request.respond(json(200, body.to_string()));
                continue;
            }

            if let Some(cmd) = url.strip_prefix("/lights/") {
                if (cmd == "on" || cmd == "off") && method == Method::Post {
                    let q = serde_json::json!({"cmd": cmd, "args": []});
                    let result = tokio::runtime::Handle::current()
                        .block_on(execute_command_json(
                            &mut ctrl_http.blocking_lock(),
                            &q,
                        ));
                    let body = match result {
                        Ok(msg) => serde_json::json!({"ok": true, "result": msg}),
                        Err(e) => serde_json::json!({"ok": false, "error": e.to_string()}),
                    };
                    let _ = request.respond(json(200, body.to_string()));
                    continue;
                }

                let parts: Vec<&str> = cmd.split('/').collect();
                if parts.len() == 2 && method == Method::Post {
                    let light_key = parts[0];
                    let sub = parts[1];

                    let mut body = String::new();
                    let _ = request.as_reader().read_to_string(&mut body);
                    let payload: serde_json::Value =
                        serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);

                    let q = http_to_command(light_key, sub, &payload);
                    let result = tokio::runtime::Handle::current()
                        .block_on(execute_command_json(
                            &mut ctrl_http.blocking_lock(),
                            &q,
                        ));
                    let resp = match result {
                        Ok(msg) => serde_json::json!({"ok": true, "result": msg}),
                        Err(e) => serde_json::json!({"ok": false, "error": e.to_string()}),
                    };
                    let _ = request.respond(json(200, resp.to_string()));
                    continue;
                }
            }

            let _ = request.respond(json(
                404,
                serde_json::json!({"ok": false, "error": "not found"}).to_string(),
            ));
        }
    });

    // Unix socket — main event loop
    let listener = tokio::net::UnixListener::bind(&sock_path)?;
    info!("daemon PID {} running", std::process::id());

    loop {
        let (stream, _) = listener.accept().await?;
        let ctrl = ctrl.clone();

        tokio::spawn(async move {
            let (reader, mut writer) = tokio::io::split(stream);
            let mut reader = BufReader::new(reader);
            let mut line = String::new();

            loop {
                line.clear();
                let n = reader.read_line(&mut line).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }

                let req: serde_json::Value = match serde_json::from_str(trimmed) {
                    Ok(v) => v,
                    Err(_) => {
                        let _ = writer
                            .write_all(
                                serde_json::json!({"error": "invalid json"})
                                    .to_string()
                                    .as_bytes(),
                            )
                            .await;
                        let _ = writer.write_all(b"\n").await;
                        continue;
                    }
                };

                let result = execute_command_json(&mut *ctrl.lock().await, &req).await;
                let out = match result {
                    Ok(msg) => serde_json::json!({"ok": true, "result": msg}),
                    Err(e) => serde_json::json!({"ok": false, "error": e.to_string()}),
                };
                let _ = writer.write_all(out.to_string().as_bytes()).await;
                let _ = writer.write_all(b"\n").await;
            }
        });
    }
}

pub async fn execute_command_json(
    ctrl: &mut MeshController,
    req: &serde_json::Value,
) -> anyhow::Result<String> {
    let cmd = req["cmd"].as_str().unwrap_or("");
    let light_key = req["light"].as_str();
    let args: Vec<String> = req["args"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let addresses = resolve_targets(ctrl, light_key)?;

    for &addr in &addresses {
        let name = ctrl
            .lights
            .iter()
            .find(|l| l.address == addr)
            .map(|l| l.name.clone())
            .unwrap_or_else(|| {
                if addr == 0xffff {
                    "all".into()
                } else {
                    format!("0x{addr:04x}")
                }
            });

        match cmd {
            "on" => {
                info!("turning {name} ON");
                ctrl.set_on_off(addr, true).await?;
            }
            "off" => {
                info!("turning {name} OFF");
                ctrl.set_on_off(addr, false).await?;
            }
            "brightness" => {
                let pct: u8 = args
                    .first()
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| anyhow::anyhow!("brightness requires a number 0-100"))?;
                info!("{name} brightness → {pct}%");
                ctrl.set_brightness(addr, pct).await?;
            }
            "cct" => {
                let b: u8 = args.first()
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| anyhow::anyhow!("cct requires brightness"))?;
                let k: u16 = args
                    .get(1)
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| anyhow::anyhow!("cct requires kelvin"))?;
                let gm: i8 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
                info!("{name} CCT → {b}%, {k}K, GM {gm}");
                ctrl.set_cct(addr, b, k, gm).await?;
            }
            "hsi" | "hsl" => {
                let b: u8 = args.first()
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| anyhow::anyhow!("hsi requires brightness"))?;
                let h: u16 = args
                    .get(1)
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| anyhow::anyhow!("hsi requires hue"))?;
                let s: u16 = args
                    .get(2)
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| anyhow::anyhow!("hsi requires saturation"))?;
                info!("{name} HSI → {b}%, hue {h}°, sat {s}%");
                ctrl.set_hsi(addr, b, h, s).await?;
            }
            "ping" => return Ok("pong".into()),
            "stop" => {
                ctrl.disconnect().await?;
                let _ = std::fs::remove_file(socket_path());
                let _ = std::fs::remove_file(pid_path());
                std::process::exit(0);
            }
            "lights" => {
                return Ok(serde_json::to_string(&ctrl.lights)?);
            }
            "help" => {
                return Ok(HELP_TEXT.to_string());
            }
            _ => anyhow::bail!("unknown command: {cmd}"),
        }
    }

    Ok("ok".into())
}

fn resolve_targets(ctrl: &MeshController, light_key: Option<&str>) -> anyhow::Result<Vec<u16>> {
    match light_key {
        None | Some("all") => Ok(vec![0xffff]),
        Some(key) => {
            let light = ctrl
                .lights
                .iter()
                .find(|l| l.key == key)
                .ok_or_else(|| {
                    let keys: Vec<&str> = ctrl.lights.iter().map(|l| l.key.as_str()).collect();
                    anyhow::anyhow!("unknown light: \"{key}\". known: {}", keys.join(", "))
                })?;
            Ok(vec![light.address])
        }
    }
}

fn parse_ha_command(payload: &serde_json::Value, light_key: &str) -> serde_json::Value {
    let state_str = payload["state"].as_str().map(|s| s.to_uppercase());
    let brightness = payload["brightness"]
        .as_f64()
        .map(|b| ((b / 255.0) * 100.0) as u8);
    let color_temp = payload["color_temp"]
        .as_f64()
        .map(|m| (1000000.0 / m).round() as u16);
    let hs_color = payload["hs_color"].as_array().map(|a| {
        (
            a[0].as_f64().unwrap_or(0.0) as u16,
            a[1].as_f64().unwrap_or(0.0) as u16,
        )
    });

    if let Some(ref s) = state_str {
        if s == "OFF" {
            return serde_json::json!({"cmd": "off", "light": light_key, "args": []});
        }
    }

    if let Some(k) = color_temp {
        let b = brightness.unwrap_or(80);
        return serde_json::json!({"cmd": "cct", "light": light_key, "args": [b.to_string(), k.to_string(), "0".to_string()]});
    }

    if let Some((h, s)) = hs_color {
        let b = brightness.unwrap_or(80);
        return serde_json::json!({"cmd": "hsi", "light": light_key, "args": [b.to_string(), h.to_string(), s.to_string()]});
    }

    if let Some(b) = brightness {
        return serde_json::json!({"cmd": "brightness", "light": light_key, "args": [b.to_string()]});
    }

    if state_str.is_some() {
        return serde_json::json!({"cmd": "on", "light": light_key, "args": []});
    }

    serde_json::json!({"cmd": "on", "light": light_key, "args": []})
}

fn http_to_command(light_key: &str, cmd: &str, body: &serde_json::Value) -> serde_json::Value {
    let light = if light_key == "all" {
        serde_json::Value::Null
    } else {
        serde_json::Value::String(light_key.into())
    };

    let args: Vec<String> = match cmd {
        "brightness" => vec![body["value"]
            .as_f64()
            .map(|v| v.to_string())
            .unwrap_or("100".into())],
        "cct" => vec![
            body["brightness"]
                .as_f64()
                .map(|v| v.to_string())
                .unwrap_or("80".into()),
            body["kelvin"]
                .as_f64()
                .map(|v| v.to_string())
                .unwrap_or("5600".into()),
            body["gm"]
                .as_f64()
                .map(|v| v.to_string())
                .unwrap_or("0".into()),
        ],
        "hsi" | "hsl" => vec![
            body["brightness"]
                .as_f64()
                .map(|v| v.to_string())
                .unwrap_or("80".into()),
            body["hue"]
                .as_f64()
                .map(|v| v.to_string())
                .unwrap_or("0".into()),
            body["saturation"]
                .as_f64()
                .map(|v| v.to_string())
                .unwrap_or("100".into()),
        ],
        _ => vec![],
    };

    serde_json::json!({"cmd": cmd, "light": light, "args": args})
}

pub const HELP_TEXT: &str = "\
commands:
  on [light]                  turn light on
  off [light]                 turn light off
  brightness <0-100> [light]  set brightness
  cct <b> <kelvin> [gm] [light]  set CCT (kelvin 2500-10000, GM -50..+50)
  hsi <b> <hue> <sat> [light]    set HSI (hue 0-360, sat 0-100)
  lights                      list configured lights
  help                        show this help
  exit / quit                 leave the REPL

light target (optional, default = all):  use the key from lights.json, e.g. key, back, fill
";
