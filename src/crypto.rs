use aes::cipher::{BlockEncrypt, KeyInit};
use aes::Aes128;
use ccm::aead::Aead;
use ccm::consts::{U13, U4};
use ccm::Ccm;

type AesCcm = Ccm<Aes128, U4, U13>;

pub fn aes_ecb_block(key: &[u8; 16], data: &[u8; 16]) -> [u8; 16] {
    let cipher = Aes128::new_from_slice(key).expect("invalid aes key");
    let mut block = (*data).into();
    cipher.encrypt_block(&mut block);
    block.into()
}

fn cmac_shift_left(b: &[u8; 16], xor_byte: u8) -> [u8; 16] {
    let mut out = [0u8; 16];
    let mut carry: u8 = 0;
    for i in (0..16).rev() {
        out[i] = (b[i] << 1) | carry;
        carry = if (b[i] & 0x80) != 0 { 1 } else { 0 };
    }
    if xor_byte != 0 {
        out[15] ^= xor_byte;
    }
    out
}

pub fn aes_cmac(key: &[u8; 16], message: &[u8]) -> [u8; 16] {
    let l = aes_ecb_block(key, &[0u8; 16]);
    let k1 = cmac_shift_left(&l, if l[0] & 0x80 != 0 { 0x87 } else { 0 });
    let k2 = cmac_shift_left(&k1, if k1[0] & 0x80 != 0 { 0x87 } else { 0 });

    let n = message.len().max(1).div_ceil(16);
    let last_complete = !message.is_empty() && message.len().is_multiple_of(16);

    let mut x = [0u8; 16];
    for i in 0..(n.saturating_sub(1)) {
        let block = &message[i * 16..i * 16 + 16];
        for j in 0..16 {
            x[j] ^= block[j];
        }
        x = aes_ecb_block(key, &x);
    }

    let mut last_block = [0u8; 16];
    if last_complete {
        let start = (n - 1) * 16;
        last_block.copy_from_slice(&message[start..start + 16]);
        for j in 0..16 {
            last_block[j] ^= k1[j];
        }
    } else {
        let rem = message.len() % 16;
        if rem > 0 {
            let start = (n - 1) * 16;
            last_block[..rem].copy_from_slice(&message[start..start + rem]);
        }
        last_block[rem] = 0x80;
        for j in 0..16 {
            last_block[j] ^= k2[j];
        }
    }

    for j in 0..16 {
        x[j] ^= last_block[j];
    }
    aes_ecb_block(key, &x)
}

pub fn s1(m: &[u8]) -> [u8; 16] {
    aes_cmac(&[0u8; 16], m)
}

pub struct DerivedNetKey {
    pub nid: u8,
    pub enc_key: [u8; 16],
    pub priv_key: [u8; 16],
}

pub fn k2(net_key: &[u8; 16]) -> DerivedNetKey {
    let salt = s1(b"smk2");
    let t = aes_cmac(&salt, net_key);
    let p = [0x00u8];

    let t1 = aes_cmac(&t, &[p.as_slice(), &[0x01]].concat());
    let t2 = aes_cmac(&t, &[t1.as_slice(), p.as_slice(), &[0x02]].concat());
    let t3 = aes_cmac(&t, &[t2.as_slice(), p.as_slice(), &[0x03]].concat());

    DerivedNetKey {
        nid: t1[15] & 0x7f,
        enc_key: t2,
        priv_key: t3,
    }
}

pub fn k4(app_key: &[u8; 16]) -> u8 {
    let salt = s1(b"smk4");
    let t = aes_cmac(&salt, app_key);
    let result = aes_cmac(&t, &[0x69, 0x64, 0x36, 0x01]);
    result[15] & 0x3f
}

pub fn aes_ccm_encrypt(key: &[u8; 16], nonce: &[u8; 13], plaintext: &[u8], mic_len: u8) -> Vec<u8> {
    use ccm::aead::generic_array::GenericArray;
    assert!(mic_len == 4, "only 4-byte MIC supported");
    let cipher = AesCcm::new_from_slice(key).expect("invalid ccm key");
    let nonce_arr = GenericArray::from_slice(nonce);
    cipher
        .encrypt(nonce_arr, plaintext)
        .expect("ccm encrypt failed")
}

pub fn compute_pecb(priv_key: &[u8; 16], iv_index: u32, privacy_random: &[u8; 7]) -> [u8; 16] {
    let mut input = [0u8; 16];
    input[5] = (iv_index >> 24) as u8;
    input[6] = (iv_index >> 16) as u8;
    input[7] = (iv_index >> 8) as u8;
    input[8] = iv_index as u8;
    input[9..16].copy_from_slice(privacy_random);
    aes_ecb_block(priv_key, &input)
}
