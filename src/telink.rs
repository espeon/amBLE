pub fn telink_payload(cmd_type: u8, cmd_value: u8) -> [u8; 10] {
    let mut p = [0u8; 10];
    p[8] = cmd_value;
    p[9] = cmd_type;
    let sum: u16 = p.iter().skip(1).map(|&b| b as u16).sum();
    p[0] = (sum & 0xff) as u8;
    p
}

pub fn telink_brightness_payload(intensity: u16) -> [u8; 10] {
    let v = intensity.min(1000);
    let mut p = [0u8; 10];
    p[7] = ((v & 3) << 6) as u8;
    p[8] = ((v >> 2) & 0xff) as u8;
    p[9] = 0x8f;
    let sum: u16 = p.iter().skip(1).map(|&b| b as u16).sum();
    p[0] = (sum & 0xff) as u8;
    p
}

pub fn telink_cct_payload(kelvin: u16, intensity: u16, gm: i8) -> [u8; 10] {
    let v = intensity.min(1000);
    let k = kelvin.max(2500).min(10000);
    let g = gm.clamp(-50, 50);
    let w12 = (k + 24) & 0x3ff;
    let gm_encoded = g.unsigned_abs() as u16;
    let gm_flag: u16 = if g < 0 { 1 } else { 0 };

    let mut p = [0u8; 10];
    p[4] = 0x40;
    p[5] = (((gm_encoded & 7) << 5) | (gm_flag << 3)) as u8;
    p[6] = (((w12 & 0xf) << 4) | ((gm_encoded >> 3) & 0xf)) as u8;
    p[7] = (((w12 >> 4) & 0x3f) | ((v & 3) << 6)) as u8;
    p[8] = ((v >> 2) & 0xff) as u8;
    p[9] = 0x82;

    let sum: u16 = p.iter().skip(1).map(|&b| b as u16).sum();
    p[0] = (sum & 0xff) as u8;
    p
}

pub fn telink_hsi_payload(hue: u16, saturation: u16, intensity: u16) -> [u8; 10] {
    let v = intensity.min(1000);
    let h = hue.min(360) & 0x1ff;
    let s = saturation.min(100) & 0x7f;

    let mut p = [0u8; 10];
    p[5] = ((s & 3) << 6) as u8;
    p[6] = (((h & 7) << 5) | ((s >> 2) & 0x1f)) as u8;
    p[7] = (((h >> 3) & 0x3f) | ((v & 3) << 6)) as u8;
    p[8] = ((v >> 2) & 0xff) as u8;
    p[9] = 0x81;

    let sum: u16 = p.iter().skip(1).map(|&b| b as u16).sum();
    p[0] = (sum & 0xff) as u8;
    p
}
