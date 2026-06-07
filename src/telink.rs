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
    let tcct = (((kelvin as u32 + 5) / 10) as u16).clamp(80, 2000);
    let g = (gm + 10).clamp(0, 20) as u64;

    let mut low: u64 = ((v & 3) as u64) << 62;
    let mut high: u16 = 0x8200 | (((v >> 2) & 0xff) as u16);

    if tcct < 1001 {
        low |= (tcct as u64) << 52;
        high |= ((tcct >> 12) & 0xff) as u16;
    } else {
        low |= (((tcct as u32 + 0x18) & 0x3ff) as u64) << 52;
        low |= 0x0000040000000000u64;
    }

    low |= 0u64 << 43; // gm_flag always 0
    low |= g << 45;

    let low_bytes = low.to_le_bytes();
    let mut p = [0u8; 10];
    p[1..8].copy_from_slice(&low_bytes[1..8]);
    p[8] = (high & 0xff) as u8;
    p[9] = ((high >> 8) & 0xff) as u8;

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

pub fn telink_rgbww_payload(
    r: u16, g: u16, b: u16, ww: u16, cw: u16, intensity: u16,
) -> [u8; 10] {
    let r = r.min(1000);
    let g = g.min(1000);
    let b = b.min(1000);
    let ww = ww.min(1000);
    let cw = cw.min(1000);
    let v = intensity.min(1000);

    let word: u64 = ((v as u64) << 12)
        | ((ww as u64) << 22)
        | ((cw as u64) << 32)
        | ((b as u64) << 42)
        | ((g as u64) << 52)
        | (((r & 3) as u64) << 62);

    let word_bytes = word.to_le_bytes();
    let mut p = [0u8; 10];
    p[1..8].copy_from_slice(&word_bytes[1..8]);
    p[8] = ((r >> 2) & 0xff) as u8;
    p[9] = 0x84;

    let sum: u16 = p.iter().skip(1).map(|&b| b as u16).sum();
    p[0] = (sum & 0xff) as u8;
    p
}

pub fn telink_gel_payload(
    cct: u16, number: u16, series: u8, brand: u8, intensity: u16,
) -> [u8; 10] {
    let v = intensity.min(1000);
    let c = cct.min(10000);

    let (cct_bits, has_offset) = if c < 1001 {
        (c as u64, false)
    } else {
        (((c as u32 + 24) & 0x3ff) as u64, true)
    };

    let mut word: u64 = (((v & 3) as u64) << 62)
        | (cct_bits << 52)
        | (((brand & 1) as u64) << 51)
        | (((series & 0xf) as u64) << 47)
        | (((number & 0x3ff) as u64) << 37);

    if has_offset {
        word |= 1u64 << 36;
    }

    let word_bytes = word.to_le_bytes();
    let mut p = [0u8; 10];
    p[1..8].copy_from_slice(&word_bytes[1..8]);
    p[8] = ((v >> 2) & 0xff) as u8;
    p[9] = 0x83;

    let sum: u16 = p.iter().skip(1).map(|&b| b as u16).sum();
    p[0] = (sum & 0xff) as u8;
    p
}

pub fn telink_xy_payload(
    x: u16, y: u16, intensity: u16,
) -> [u8; 10] {
    let v = intensity.min(1000);

    let word: u64 = ((x as u64 & 0x3fff) << 48)
        | ((y as u64 & 0x3fff) << 34)
        | (((v & 3) as u64) << 62);

    let word_bytes = word.to_le_bytes();
    let mut p = [0u8; 10];
    p[1..8].copy_from_slice(&word_bytes[1..8]);
    p[8] = ((v >> 2) & 0xff) as u8;
    p[9] = 0x85;

    let sum: u16 = p.iter().skip(1).map(|&b| b as u16).sum();
    p[0] = (sum & 0xff) as u8;
    p
}

pub fn telink_dim_curve_payload(curve: u8) -> [u8; 10] {
    let mut p = [0u8; 10];
    p[8] = curve;
    p[9] = 0x88;

    let sum: u16 = p.iter().skip(1).map(|&b| b as u16).sum();
    p[0] = (sum & 0xff) as u8;
    p
}

pub fn telink_read_data_payload(cmd_type: u8) -> [u8; 10] {
    let mut p = [0u8; 10];
    p[8] = 0x00;
    p[9] = cmd_type;

    let sum: u16 = p.iter().skip(1).map(|&b| b as u16).sum();
    p[0] = (sum & 0xff) as u8;
    p
}


