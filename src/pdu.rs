use crate::crypto::{aes_ccm_encrypt, compute_pecb};

pub const LOCAL_ADDRESS: u16 = 0x0001;
pub const DEFAULT_TTL: u8 = 10;
pub const GROUP_ALL: u16 = 0xc000;

pub const PROXY_TYPE_NETWORK: u8 = 0x00;
pub const PROXY_TYPE_BEACON: u8 = 0x01;
pub const PROXY_TYPE_PROXY_CONFIG: u8 = 0x02;

pub const PROXY_CFG_SET_FILTER_TYPE: u8 = 0x00;
pub const PROXY_CFG_ADD_ADDRESSES: u8 = 0x01;
pub const PROXY_FILTER_WHITELIST: u8 = 0x00;

fn opcode_bytes(opcode: u32) -> Vec<u8> {
    let first_byte = (opcode & 0xff) as u8;
    if first_byte < 0x80 {
        vec![first_byte]
    } else if first_byte < 0xc0 {
        vec![first_byte, ((opcode >> 8) & 0xff) as u8]
    } else {
        vec![first_byte, ((opcode >> 8) & 0xff) as u8, ((opcode >> 16) & 0xff) as u8]
    }
}

fn app_nonce(seq: u32, src: u16, dst: u16, iv_index: u32) -> [u8; 13] {
    [
        0x01,
        0x00,
        (seq >> 16) as u8,
        (seq >> 8) as u8,
        seq as u8,
        (src >> 8) as u8,
        src as u8,
        (dst >> 8) as u8,
        dst as u8,
        (iv_index >> 24) as u8,
        (iv_index >> 16) as u8,
        (iv_index >> 8) as u8,
        iv_index as u8,
    ]
}

fn net_nonce(ctl: u8, ttl: u8, seq: u32, src: u16, iv_index: u32) -> [u8; 13] {
    [
        0x00,
        (ctl << 7) | (ttl & 0x7f),
        (seq >> 16) as u8,
        (seq >> 8) as u8,
        seq as u8,
        (src >> 8) as u8,
        src as u8,
        0x00,
        0x00,
        (iv_index >> 24) as u8,
        (iv_index >> 16) as u8,
        (iv_index >> 8) as u8,
        iv_index as u8,
    ]
}

fn proxy_nonce(seq: u32, src: u16, iv_index: u32) -> [u8; 13] {
    [
        0x03,
        0x00,
        (seq >> 16) as u8,
        (seq >> 8) as u8,
        seq as u8,
        (src >> 8) as u8,
        src as u8,
        0x00,
        0x00,
        (iv_index >> 24) as u8,
        (iv_index >> 16) as u8,
        (iv_index >> 8) as u8,
        iv_index as u8,
    ]
}

#[allow(clippy::too_many_arguments)]
pub fn build_proxy_pdu(
    app_key: &[u8; 16],
    aid: u8,
    nid: u8,
    enc_key: &[u8; 16],
    priv_key: &[u8; 16],
    seq: u32,
    src: u16,
    dst: u16,
    iv_index: u32,
    opcode: u32,
    params: &[u8],
) -> Vec<u8> {
    let access_pdu = [opcode_bytes(opcode), params.to_vec()].concat();

    let upper_encrypted = aes_ccm_encrypt(
        app_key,
        &app_nonce(seq, src, dst, iv_index),
        &access_pdu,
        4,
    );

    let lower_transport = [
        [0x40 | (aid & 0x3f)].as_slice(),
        upper_encrypted.as_slice(),
    ]
    .concat();

    let net_payload = [
        [(dst >> 8) as u8, dst as u8].as_slice(),
        lower_transport.as_slice(),
    ]
    .concat();

    let encrypted_payload = aes_ccm_encrypt(
        enc_key,
        &net_nonce(0, DEFAULT_TTL, seq, src, iv_index),
        &net_payload,
        4,
    );

    let pecb = compute_pecb(priv_key, iv_index, &encrypted_payload[..7].try_into().unwrap());
    let hdr = [
        (DEFAULT_TTL & 0x7f),
        (seq >> 16) as u8,
        (seq >> 8) as u8,
        seq as u8,
        (src >> 8) as u8,
        src as u8,
    ];
    let mut obfuscated = [0u8; 6];
    for i in 0..6 {
        obfuscated[i] = hdr[i] ^ pecb[i];
    }

    let ivi = (iv_index & 1) as u8;
    let network_pdu = [
        [(ivi << 7) | (nid & 0x7f)].as_slice(),
        obfuscated.as_slice(),
        encrypted_payload.as_slice(),
    ]
    .concat();

    [[PROXY_TYPE_NETWORK].as_slice(), network_pdu.as_slice()].concat()
}

#[allow(clippy::too_many_arguments)]
pub fn build_proxy_config_pdu(
    nid: u8,
    enc_key: &[u8; 16],
    priv_key: &[u8; 16],
    seq: u32,
    src: u16,
    iv_index: u32,
    opcode: u8,
    params: &[u8],
) -> Vec<u8> {
    let transport_pdu = [[opcode & 0x7f].as_slice(), params].concat();
    let net_payload = [[0x00, 0x00].as_slice(), transport_pdu.as_slice()].concat();

    let encrypted = aes_ccm_encrypt(
        enc_key,
        &proxy_nonce(seq, src, iv_index),
        &net_payload,
        4,
    );

    let pecb = compute_pecb(priv_key, iv_index, &encrypted[..7].try_into().unwrap());
    let hdr = [
        (1u8 << 7),
        (seq >> 16) as u8,
        (seq >> 8) as u8,
        seq as u8,
        (src >> 8) as u8,
        src as u8,
    ];
    let mut obfuscated = [0u8; 6];
    for i in 0..6 {
        obfuscated[i] = hdr[i] ^ pecb[i];
    }

    let ivi = (iv_index & 1) as u8;
    let network_pdu = [
        [(ivi << 7) | (nid & 0x7f)].as_slice(),
        obfuscated.as_slice(),
        encrypted.as_slice(),
    ]
    .concat();

    [[PROXY_TYPE_PROXY_CONFIG].as_slice(), network_pdu.as_slice()].concat()
}

pub fn parse_iv_index(data: &[u8]) -> Option<u32> {
    if data.len() < 23 {
        return None;
    }
    let pdu_type = data[0] & 0x3f;
    if pdu_type != PROXY_TYPE_BEACON {
        return None;
    }
    if data[1] != 0x01 {
        return None;
    }
    Some(u32::from_be_bytes([
        data[11], data[12], data[13], data[14],
    ]))
}
