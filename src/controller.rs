use crate::config::{Config, LightConfig};
use crate::crypto::{k2, k4, DerivedNetKey};
use crate::pdu::{
    build_proxy_config_pdu, build_proxy_pdu, parse_iv_index, GROUP_ALL, LOCAL_ADDRESS,
    PROXY_CFG_ADD_ADDRESSES, PROXY_CFG_SET_FILTER_TYPE, PROXY_FILTER_WHITELIST,
};
use crate::telink::{
    telink_brightness_payload, telink_cct_payload, telink_hsi_payload, telink_payload,
    telink_rgbww_payload,
};
use btleplug::api::{Central, Manager as _, Peripheral as _, ScanFilter, WriteType};
use btleplug::platform::{Adapter, Manager, Peripheral};
use futures::stream::StreamExt;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;
use tokio::sync::broadcast;
use tokio::time::{sleep, timeout};
use tracing::{error, info};

const PROXY_DATA_IN: uuid::Uuid = uuid::uuid!("00002add-0000-1000-8000-00805f9b34fb");
const PROXY_DATA_OUT: uuid::Uuid = uuid::uuid!("00002ade-0000-1000-8000-00805f9b34fb");
const BATTERY_LEVEL: uuid::Uuid = uuid::uuid!("00002a19-0000-1000-8000-00805f9b34fb");

pub struct MeshController {
    adapter: Adapter,
    peripheral: Option<Peripheral>,
    data_in: Option<btleplug::api::Characteristic>,
    data_out: Option<btleplug::api::Characteristic>,
    battery: Option<btleplug::api::Characteristic>,
    iv_index: Arc<AtomicU32>,
    seq: u32,
    derived: DerivedNetKey,
    aid: u8,
    app_key: [u8; 16],
    relay_hub: String,
    pub lights: Vec<LightConfig>,
    beacon_notify: Arc<Notify>,
    filter_notify: Arc<Notify>,
    response_tx: broadcast::Sender<Vec<u8>>,
    response_rx: broadcast::Receiver<Vec<u8>>,
}

impl MeshController {
    pub async fn new(config: Config) -> anyhow::Result<Self> {
        let net_key = hex::decode(&config.net_key)
            .map_err(|e| anyhow::anyhow!("invalid netKey hex: {e}"))?;
        let app_key = hex::decode(&config.app_key)
            .map_err(|e| anyhow::anyhow!("invalid appKey hex: {e}"))?;

        let net_key: [u8; 16] = net_key
            .try_into()
            .map_err(|_| anyhow::anyhow!("netKey must be 16 bytes"))?;
        let app_key_arr: [u8; 16] = app_key
            .try_into()
            .map_err(|_| anyhow::anyhow!("appKey must be 16 bytes"))?;

        let derived = k2(&net_key);
        let aid = k4(&app_key_arr);

        let manager = Manager::new().await?;
        let adapters = manager.adapters().await?;
        let adapter = adapters
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("no bluetooth adapter found"))?;

        let seq = 12000000 + rand::random::<u32>() % 4000000;
        let (response_tx, response_rx) = broadcast::channel(32);

        Ok(Self {
            adapter,
            peripheral: None,
            data_in: None,
            data_out: None,
            battery: None,
            iv_index: Arc::new(AtomicU32::new(0)),
            seq,
            derived,
            aid,
            app_key: app_key_arr,
            relay_hub: config.relay_hub.to_lowercase().replace('-', ":"),
            lights: config.lights,
            beacon_notify: Arc::new(Notify::new()),
            filter_notify: Arc::new(Notify::new()),
            response_tx,
            response_rx,
        })
    }

    fn normalize_addr(addr: &str) -> String {
        addr.to_lowercase().replace('-', ":")
    }

    pub async fn connect(&mut self) -> anyhow::Result<bool> {
        let hub_mac = Self::normalize_addr(&self.relay_hub);
        let known_macs: Vec<String> =
            self.lights.iter().map(|l| Self::normalize_addr(&l.mac)).collect();

        info!("scanning for lights (relay hub: {hub_mac})...");

        self.adapter.start_scan(ScanFilter::default()).await?;

        let mut best: Option<Peripheral> = None;
        let start = std::time::Instant::now();
        let preferred_uuids = [
            "b3ed1263a9304e5132b3edfbb4c71aec",
            "d16927ee947b5a0ced73358c29bc4bcd",
        ];

        loop {
            let elapsed = start.elapsed();
            let periphs = self.adapter.peripherals().await.unwrap_or_default();

            for p in &periphs {
                let props = match p.properties().await {
                    Ok(Some(props)) => props,
                    _ => continue,
                };

                let name = props.local_name.as_deref().unwrap_or("").to_lowercase();
                let addr_str = p.id().to_string().to_lowercase().replace('-', ":");
                let has_proxy = props
                    .services
                    .iter()
                    .any(|s| s.to_string().to_lowercase().contains("1828"));
                let is_by_name =
                    name.contains("amaran") || name.contains("aputure") || name.contains("slck");
                let is_by_mac = known_macs.contains(&addr_str);

                if !has_proxy && !is_by_name && !is_by_mac {
                    continue;
                }

                let is_hub = addr_str == hub_mac
                    || (has_proxy && name == self.relay_hub.to_lowercase());

                if is_hub {
                    info!("found relay hub: {name} ({addr_str})");
                    best = Some(p.clone());
                    break;
                }

                if best.is_none() {
                    best = Some(p.clone());
                }
            }

            if best.is_some() && elapsed > Duration::from_secs(4) {
                break;
            }
            if elapsed > Duration::from_secs(5) {
                for p in &periphs {
                    let id = p.id().to_string().to_lowercase().replace('-', "");
                    if preferred_uuids.contains(&id.as_str()) {
                        best = Some(p.clone());
                        break;
                    }
                }
                if best.is_some() {
                    break;
                }
            }
            if elapsed > Duration::from_secs(15) {
                break;
            }

            sleep(Duration::from_millis(300)).await;
        }

        self.adapter.stop_scan().await?;

        let peripheral = match best {
            Some(p) => p,
            None => {
                error!("no Amaran lights found — is the Amaran Desktop app closed?");
                return Ok(false);
            }
        };

        let props = peripheral.properties().await?.unwrap_or_default();
        let name = props.local_name.as_deref().unwrap_or("unknown");

        info!("connecting to {name}...");
        peripheral.connect().await?;
        peripheral.discover_services().await?;

        let chars = peripheral.characteristics();

        let data_in = chars.iter().find(|c| c.uuid == PROXY_DATA_IN).cloned();
        let data_out = chars.iter().find(|c| c.uuid == PROXY_DATA_OUT).cloned();
        let battery = chars.iter().find(|c| c.uuid == BATTERY_LEVEL).cloned();

        let (data_in, data_out) = match (data_in, data_out) {
            (Some(di), Some(do_)) => (di, do_),
            _ => {
                error!(
                    "mesh proxy characteristics not found on {name}. \
                     run: cargo run -- scan connect {}",
                    peripheral.id()
                );
                peripheral.disconnect().await?;
                return Ok(false);
            }
        };

        peripheral.subscribe(&data_out).await?;

        let iv_index = self.iv_index.clone();
        let beacon_notify = self.beacon_notify.clone();
        let filter_notify = self.filter_notify.clone();
        let response_tx = self.response_tx.clone();

        let p_notif = peripheral.clone();
        tokio::spawn(async move {
            let mut stream = match p_notif.notifications().await {
                Ok(s) => s,
                Err(_) => return,
            };
            while let Some(data) = stream.next().await {
                if data.uuid != PROXY_DATA_OUT {
                    continue;
                }
                let pdu_type = data.value.first().copied().unwrap_or(0) & 0x3f;
                if pdu_type == 0x02 {
                    filter_notify.notify_one();
                }
                if let Some(iv) = parse_iv_index(&data.value) {
                    iv_index.store(iv, Ordering::SeqCst);
                    info!("← Secure Network Beacon — IV Index: 0x{iv:08x}");
                    beacon_notify.notify_one();
                }
                if pdu_type == 0x00 {
                    let _ = response_tx.send(data.value.clone());
                }
            }
        });

        self.peripheral = Some(peripheral);
        self.data_in = Some(data_in);
        self.data_out = Some(data_out);
        self.battery = battery;

        info!("connected to {name}");
        Ok(true)
    }

    fn iv(&self) -> u32 {
        self.iv_index.load(Ordering::SeqCst)
    }

    pub async fn wait_for_beacon(&self, timeout_ms: u64) -> u32 {
        match timeout(
            Duration::from_millis(timeout_ms),
            self.beacon_notify.notified(),
        )
        .await
        {
            Ok(()) => self.iv(),
            Err(_) => {
                info!("no beacon received; using IV index 0");
                0
            }
        }
    }

    pub async fn setup_proxy_filter(&mut self) -> anyhow::Result<()> {
        let seq1 = self.next_seq();
        let seq2 = self.next_seq();
        let iv = self.iv();

        sleep(Duration::from_millis(500)).await;

        let set_filter = build_proxy_config_pdu(
            self.derived.nid,
            &self.derived.enc_key,
            &self.derived.priv_key,
            seq1,
            LOCAL_ADDRESS,
            iv,
            PROXY_CFG_SET_FILTER_TYPE,
            &[PROXY_FILTER_WHITELIST],
        );

        info!(
            "→ Proxy: Set Filter Type = Whitelist  {}",
            hex::encode(&set_filter)
        );

        self.peripheral
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("not connected"))?
            .write(
                self.data_in
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("not connected"))?,
                &set_filter,
                WriteType::WithoutResponse,
            )
            .await?;

        let _ = timeout(Duration::from_secs(2), self.filter_notify.notified()).await;

        let mut addresses: Vec<u16> = vec![LOCAL_ADDRESS, 0xffff, GROUP_ALL];
        for light in &self.lights {
            addresses.push(light.address);
        }
        let mut addr_buf = Vec::with_capacity(addresses.len() * 2);
        for a in &addresses {
            addr_buf.extend_from_slice(&a.to_be_bytes());
        }

        let add_addr = build_proxy_config_pdu(
            self.derived.nid,
            &self.derived.enc_key,
            &self.derived.priv_key,
            seq2,
            LOCAL_ADDRESS,
            iv,
            PROXY_CFG_ADD_ADDRESSES,
            &addr_buf,
        );

        let addr_strs: Vec<String> = addresses.iter().map(|a| format!("0x{a:04x}")).collect();
        info!(
            "→ Proxy: Add Addresses [{}]  {}",
            addr_strs.join(", "),
            hex::encode(&add_addr)
        );

        self.peripheral
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("not connected"))?
            .write(
                self.data_in
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("not connected"))?,
                &add_addr,
                WriteType::WithoutResponse,
            )
            .await?;

        sleep(Duration::from_millis(300)).await;
        info!("proxy filter configured — ready");
        Ok(())
    }

    fn next_seq(&mut self) -> u32 {
        self.seq = (self.seq + 1) & 0xffffff;
        self.seq
    }

    async fn write_with_timeout(&self, data: &[u8], timeout_ms: u64) -> anyhow::Result<()> {
        let peripheral = self
            .peripheral
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("not connected"))?;
        let data_in = self
            .data_in
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("not connected"))?;

        match timeout(
            Duration::from_millis(timeout_ms),
            peripheral.write(data_in, data, WriteType::WithoutResponse),
        )
        .await
        {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(e.into()),
            Err(_) => Ok(()),
        }
    }

    pub async fn send(
        &mut self,
        dst: u16,
        opcode: u32,
        params: &[u8],
        retries: u32,
    ) -> anyhow::Result<()> {
        let iv = self.iv();
        for i in 0..retries {
            let seq = self.next_seq();
            let pdu = build_proxy_pdu(
                &self.app_key,
                self.aid,
                self.derived.nid,
                &self.derived.enc_key,
                &self.derived.priv_key,
                seq,
                LOCAL_ADDRESS,
                dst,
                iv,
                opcode,
                params,
            );
            if i == 0 {
                info!(
                    "  → dst=0x{dst:04x} opcode=0x{opcode:04x} try={}/{} payload={}",
                    i + 1,
                    retries,
                    hex::encode(&pdu)
                );
            } else {
                info!("    retry {} seq={seq} payload={}", i + 1, hex::encode(&pdu));
            }
            self.write_with_timeout(&pdu, 1500).await?;
            if i + 1 < retries {
                sleep(Duration::from_millis(80)).await;
            }
        }
        Ok(())
    }

    pub async fn set_on_off(&mut self, dst: u16, on: bool) -> anyhow::Result<()> {
        let params = telink_payload(0x8c, if on { 0x01 } else { 0x00 });
        self.send(dst, 0x26, &params, 3).await
    }

    pub async fn set_brightness(&mut self, dst: u16, percent: u8) -> anyhow::Result<()> {
        let intensity = (percent as u16 * 10).min(1000);
        let params = telink_brightness_payload(intensity);
        self.send(dst, 0x26, &params, 3).await
    }

    pub async fn set_cct(
        &mut self,
        dst: u16,
        brightness_percent: u8,
        kelvin: u16,
        gm: i8,
    ) -> anyhow::Result<()> {
        let intensity = (brightness_percent as u16 * 10).min(1000);
        let params = telink_cct_payload(kelvin, intensity, gm);
        self.send(dst, 0x26, &params, 3).await
    }

    pub async fn set_hsi(
        &mut self,
        dst: u16,
        brightness_percent: u8,
        hue: u16,
        saturation: u16,
    ) -> anyhow::Result<()> {
        let intensity = (brightness_percent as u16 * 10).min(1000);
        let params = telink_hsi_payload(hue, saturation, intensity);
        self.send(dst, 0x26, &params, 3).await
    }

    pub async fn set_rgbww(
        &mut self,
        dst: u16,
        r: u16,
        g: u16,
        b: u16,
        ww: u16,
        cw: u16,
        intensity: u16,
    ) -> anyhow::Result<()> {
        let params = telink_rgbww_payload(r, g, b, ww, cw, intensity);
        self.send(dst, 0x26, &params, 3).await
    }

    pub async fn query_status(&mut self, dst: u16) -> anyhow::Result<Vec<u8>> {
        self.response_rx = self.response_tx.subscribe();
        self.send(dst, 0x26, &telink_payload(0xcf, 0x01), 1).await?;
        match timeout(Duration::from_secs(3), self.response_rx.recv()).await {
            Ok(Ok(data)) => Ok(data),
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => {
                anyhow::bail!("response channel lagged")
            }
            Ok(Err(broadcast::error::RecvError::Closed)) => {
                anyhow::bail!("response channel closed")
            }
            Err(_) => anyhow::bail!("status query timed out"),
        }
    }

    pub async fn read_battery(&self) -> anyhow::Result<Option<u8>> {
        match (&self.peripheral, &self.battery) {
            (Some(p), Some(ch)) => {
                let val = p.read(ch).await?;
                Ok(val.first().copied())
            }
            _ => Ok(None),
        }
    }

    pub async fn disconnect(&mut self) -> anyhow::Result<()> {
        if let Some(ref p) = self.peripheral {
            let _ = p.disconnect().await;
            info!("disconnected");
        }
        Ok(())
    }
}
