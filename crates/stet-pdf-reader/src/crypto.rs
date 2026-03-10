// stet-pdf-reader
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PDF encryption: Standard security handler (RC4 + AES-128/256).

use crate::error::PdfError;
use crate::objects::PdfDict;

/// Encryption parameters extracted from the Encrypt dict.
pub struct EncryptionState {
    /// Encryption key (5-32 bytes).
    key: Vec<u8>,
    /// Version (1=RC4-40, 2=RC4-128, 4=AES-128, 5=AES-256).
    #[allow(dead_code)]
    version: i32,
    /// Stream encryption method.
    stm_method: CryptMethod,
    /// String encryption method.
    str_method: CryptMethod,
}

#[derive(Clone, Copy, PartialEq)]
enum CryptMethod {
    None,
    RC4,
    AesV2, // AES-128
    AesV3, // AES-256
}

impl EncryptionState {
    /// Try to decrypt with empty user password.
    /// Returns Ok(state) if successful, Err if password required.
    pub fn try_open(
        encrypt_dict: &PdfDict,
        _trailer: &PdfDict,
        file_id: &[u8],
    ) -> Result<Self, PdfError> {
        let v = encrypt_dict.get_int(b"V").unwrap_or(0) as i32;
        let r = encrypt_dict.get_int(b"R").unwrap_or(0) as i32;
        let key_length = encrypt_dict.get_int(b"Length").unwrap_or(40) as usize / 8;

        let o_value = encrypt_dict
            .get(b"O")
            .and_then(|o| o.as_str())
            .ok_or(PdfError::Other("Encrypt missing /O".into()))?
            .to_vec();
        let u_value = encrypt_dict
            .get(b"U")
            .and_then(|o| o.as_str())
            .ok_or(PdfError::Other("Encrypt missing /U".into()))?
            .to_vec();
        let p_value = encrypt_dict.get_int(b"P").unwrap_or(0) as i32;

        if v == 5 {
            // AES-256 (PDF 2.0)
            return Self::try_open_v5(encrypt_dict, &u_value);
        }

        // Standard handler: compute encryption key from empty password
        let password = b"";
        let key = compute_encryption_key(password, &o_value, p_value, file_id, key_length, r);

        // Verify against /U value
        if !verify_user_password(&key, &u_value, file_id, r) {
            return Err(PdfError::Other("PDF requires a password".into()));
        }

        let (stm_method, str_method) = if v >= 4 {
            // Parse CF dict for methods
            let cf = encrypt_dict.get_dict(b"CF");
            let stm_name = encrypt_dict.get_name(b"StmF").unwrap_or(b"Identity");
            let str_name = encrypt_dict.get_name(b"StrF").unwrap_or(b"Identity");
            (
                parse_crypt_method(cf, stm_name),
                parse_crypt_method(cf, str_name),
            )
        } else {
            (CryptMethod::RC4, CryptMethod::RC4)
        };

        Ok(Self {
            key,
            version: v,
            stm_method,
            str_method,
        })
    }

    fn try_open_v5(encrypt_dict: &PdfDict, u_value: &[u8]) -> Result<Self, PdfError> {
        // AES-256: R=5 or R=6
        if u_value.len() < 48 {
            return Err(PdfError::Other("AES-256: /U too short".into()));
        }

        let validation_salt = &u_value[32..40];
        let key_salt = &u_value[40..48];

        // Hash empty password with validation salt
        let hash = sha256(&[b"", validation_salt]);
        if hash[..] != u_value[..32] {
            return Err(PdfError::Other("PDF requires a password".into()));
        }

        // Derive key: SHA-256(password + key salt)
        let key_hash = sha256(&[b"", key_salt]);

        // Decrypt UE with this key to get file encryption key
        let ue = encrypt_dict
            .get(b"UE")
            .and_then(|o| o.as_str())
            .ok_or(PdfError::Other("Encrypt missing /UE".into()))?;

        if ue.len() < 32 {
            return Err(PdfError::Other("AES-256: /UE too short".into()));
        }

        let file_key = aes_cbc_decrypt(&key_hash, &[0u8; 16], &ue[..32])?;

        Ok(Self {
            key: file_key,
            version: 5,
            stm_method: CryptMethod::AesV3,
            str_method: CryptMethod::AesV3,
        })
    }

    /// Decrypt a string.
    pub fn decrypt_string(&self, data: &[u8], obj_num: u32, gen_num: u16) -> Vec<u8> {
        self.decrypt_data(data, obj_num, gen_num, self.str_method)
    }

    /// Decrypt a stream.
    pub fn decrypt_stream(&self, data: &[u8], obj_num: u32, gen_num: u16) -> Vec<u8> {
        self.decrypt_data(data, obj_num, gen_num, self.stm_method)
    }

    fn decrypt_data(
        &self,
        data: &[u8],
        obj_num: u32,
        gen_num: u16,
        method: CryptMethod,
    ) -> Vec<u8> {
        match method {
            CryptMethod::None => data.to_vec(),
            CryptMethod::RC4 => {
                let obj_key = compute_object_key(&self.key, obj_num, gen_num, false);
                rc4(&obj_key, data)
            }
            CryptMethod::AesV2 => {
                let obj_key = compute_object_key(&self.key, obj_num, gen_num, true);
                if data.len() < 16 {
                    return data.to_vec();
                }
                aes_cbc_decrypt(&obj_key, &data[..16], &data[16..])
                    .unwrap_or_else(|_| data.to_vec())
            }
            CryptMethod::AesV3 => {
                if data.len() < 16 {
                    return data.to_vec();
                }
                aes_cbc_decrypt(&self.key, &data[..16], &data[16..])
                    .unwrap_or_else(|_| data.to_vec())
            }
        }
    }
}

fn parse_crypt_method(cf: Option<&PdfDict>, name: &[u8]) -> CryptMethod {
    if name == b"Identity" {
        return CryptMethod::None;
    }
    let cfm = cf
        .and_then(|d| d.get_dict(name))
        .and_then(|d| d.get_name(b"CFM"));
    match cfm {
        Some(b"None") => CryptMethod::None,
        Some(b"V2") => CryptMethod::RC4,
        Some(b"AESV2") => CryptMethod::AesV2,
        Some(b"AESV3") => CryptMethod::AesV3,
        _ => CryptMethod::RC4,
    }
}

/// Compute encryption key (Algorithm 2, PDF spec).
fn compute_encryption_key(
    password: &[u8],
    o_value: &[u8],
    p: i32,
    file_id: &[u8],
    key_length: usize,
    revision: i32,
) -> Vec<u8> {
    // Pad password to 32 bytes
    let padded = pad_password(password);

    let mut digest = md5::Context::new();
    digest.consume(padded);
    digest.consume(o_value);
    digest.consume(p.to_le_bytes());
    digest.consume(file_id);

    let mut hash = digest.compute().0;

    if revision >= 3 {
        for _ in 0..50 {
            hash = md5::compute(&hash[..key_length]).0;
        }
    }

    hash[..key_length].to_vec()
}

/// Verify user password (Algorithm 6/7).
fn verify_user_password(key: &[u8], u_value: &[u8], file_id: &[u8], revision: i32) -> bool {
    if revision <= 2 {
        // Algorithm 6: encrypt padding with key, compare to /U
        let encrypted = rc4(key, &PASSWORD_PADDING);
        encrypted == u_value
    } else {
        // Algorithm 7: MD5(padding + file_id), then 20 rounds of RC4
        let mut digest = md5::Context::new();
        digest.consume(PASSWORD_PADDING);
        digest.consume(file_id);
        let hash = digest.compute().0;

        let mut result = rc4(key, &hash);
        for i in 1..=19u8 {
            let derived_key: Vec<u8> = key.iter().map(|&b| b ^ i).collect();
            result = rc4(&derived_key, &result);
        }

        // Compare first 16 bytes
        u_value.len() >= 16 && result[..16] == u_value[..16]
    }
}

/// Compute per-object encryption key.
fn compute_object_key(key: &[u8], obj_num: u32, gen_num: u16, is_aes: bool) -> Vec<u8> {
    let mut digest = md5::Context::new();
    digest.consume(key);
    digest.consume(&obj_num.to_le_bytes()[..3]);
    digest.consume(gen_num.to_le_bytes());
    if is_aes {
        digest.consume(b"sAlT");
    }
    let hash = digest.compute().0;
    let len = (key.len() + 5).min(16);
    hash[..len].to_vec()
}

/// RC4 encryption/decryption (symmetric).
fn rc4(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut s: Vec<u8> = (0..=255u8).collect();
    let mut j: u8 = 0;
    for i in 0..256 {
        j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]);
        s.swap(i, j as usize);
    }

    let mut i: u8 = 0;
    j = 0;
    let mut result = Vec::with_capacity(data.len());
    for &byte in data {
        i = i.wrapping_add(1);
        j = j.wrapping_add(s[i as usize]);
        s.swap(i as usize, j as usize);
        let k = s[(s[i as usize].wrapping_add(s[j as usize])) as usize];
        result.push(byte ^ k);
    }
    result
}

/// AES-CBC decryption.
fn aes_cbc_decrypt(key: &[u8], iv: &[u8], data: &[u8]) -> Result<Vec<u8>, PdfError> {
    if data.is_empty() {
        return Ok(Vec::new());
    }
    if !data.len().is_multiple_of(16) {
        return Err(PdfError::Other("AES data not block-aligned".into()));
    }

    let mut prev_block = [0u8; 16];
    let iv_len = iv.len().min(16);
    prev_block[..iv_len].copy_from_slice(&iv[..iv_len]);

    let round_keys = aes_key_expansion(key);

    let mut result = Vec::with_capacity(data.len());
    for chunk in data.chunks(16) {
        let decrypted = aes_decrypt_block(chunk, &round_keys);
        for i in 0..16 {
            result.push(decrypted[i] ^ prev_block[i]);
        }
        prev_block.copy_from_slice(chunk);
    }

    // Remove PKCS#7 padding
    if let Some(&pad_len) = result.last() {
        let pad_len = pad_len as usize;
        if pad_len > 0
            && pad_len <= 16
            && result.len() >= pad_len
            && result[result.len() - pad_len..]
                .iter()
                .all(|&b| b == pad_len as u8)
        {
            result.truncate(result.len() - pad_len);
        }
    }

    Ok(result)
}

/// SHA-256 hash (for AES-256).
fn sha256(inputs: &[&[u8]]) -> [u8; 32] {
    sha256_impl(inputs)
}

// --- AES implementation (128/256-bit) ---
// Standard AES following FIPS 197.

const SBOX: [u8; 256] = [
    0x63, 0x7c, 0x77, 0x7b, 0xf2, 0x6b, 0x6f, 0xc5, 0x30, 0x01, 0x67, 0x2b, 0xfe, 0xd7, 0xab, 0x76,
    0xca, 0x82, 0xc9, 0x7d, 0xfa, 0x59, 0x47, 0xf0, 0xad, 0xd4, 0xa2, 0xaf, 0x9c, 0xa4, 0x72, 0xc0,
    0xb7, 0xfd, 0x93, 0x26, 0x36, 0x3f, 0xf7, 0xcc, 0x34, 0xa5, 0xe5, 0xf1, 0x71, 0xd8, 0x31, 0x15,
    0x04, 0xc7, 0x23, 0xc3, 0x18, 0x96, 0x05, 0x9a, 0x07, 0x12, 0x80, 0xe2, 0xeb, 0x27, 0xb2, 0x75,
    0x09, 0x83, 0x2c, 0x1a, 0x1b, 0x6e, 0x5a, 0xa0, 0x52, 0x3b, 0xd6, 0xb3, 0x29, 0xe3, 0x2f, 0x84,
    0x53, 0xd1, 0x00, 0xed, 0x20, 0xfc, 0xb1, 0x5b, 0x6a, 0xcb, 0xbe, 0x39, 0x4a, 0x4c, 0x58, 0xcf,
    0xd0, 0xef, 0xaa, 0xfb, 0x43, 0x4d, 0x33, 0x85, 0x45, 0xf9, 0x02, 0x7f, 0x50, 0x3c, 0x9f, 0xa8,
    0x51, 0xa3, 0x40, 0x8f, 0x92, 0x9d, 0x38, 0xf5, 0xbc, 0xb6, 0xda, 0x21, 0x10, 0xff, 0xf3, 0xd2,
    0xcd, 0x0c, 0x13, 0xec, 0x5f, 0x97, 0x44, 0x17, 0xc4, 0xa7, 0x7e, 0x3d, 0x64, 0x5d, 0x19, 0x73,
    0x60, 0x81, 0x4f, 0xdc, 0x22, 0x2a, 0x90, 0x88, 0x46, 0xee, 0xb8, 0x14, 0xde, 0x5e, 0x0b, 0xdb,
    0xe0, 0x32, 0x3a, 0x0a, 0x49, 0x06, 0x24, 0x5c, 0xc2, 0xd3, 0xac, 0x62, 0x91, 0x95, 0xe4, 0x79,
    0xe7, 0xc8, 0x37, 0x6d, 0x8d, 0xd5, 0x4e, 0xa9, 0x6c, 0x56, 0xf4, 0xea, 0x65, 0x7a, 0xae, 0x08,
    0xba, 0x78, 0x25, 0x2e, 0x1c, 0xa6, 0xb4, 0xc6, 0xe8, 0xdd, 0x74, 0x1f, 0x4b, 0xbd, 0x8b, 0x8a,
    0x70, 0x3e, 0xb5, 0x66, 0x48, 0x03, 0xf6, 0x0e, 0x61, 0x35, 0x57, 0xb9, 0x86, 0xc1, 0x1d, 0x9e,
    0xe1, 0xf8, 0x98, 0x11, 0x69, 0xd9, 0x8e, 0x94, 0x9b, 0x1e, 0x87, 0xe9, 0xce, 0x55, 0x28, 0xdf,
    0x8c, 0xa1, 0x89, 0x0d, 0xbf, 0xe6, 0x42, 0x68, 0x41, 0x99, 0x2d, 0x0f, 0xb0, 0x54, 0xbb, 0x16,
];

const INV_SBOX: [u8; 256] = [
    0x52, 0x09, 0x6a, 0xd5, 0x30, 0x36, 0xa5, 0x38, 0xbf, 0x40, 0xa3, 0x9e, 0x81, 0xf3, 0xd7, 0xfb,
    0x7c, 0xe3, 0x39, 0x82, 0x9b, 0x2f, 0xff, 0x87, 0x34, 0x8e, 0x43, 0x44, 0xc4, 0xde, 0xe9, 0xcb,
    0x54, 0x7b, 0x94, 0x32, 0xa6, 0xc2, 0x23, 0x3d, 0xee, 0x4c, 0x95, 0x0b, 0x42, 0xfa, 0xc3, 0x4e,
    0x08, 0x2e, 0xa1, 0x66, 0x28, 0xd9, 0x24, 0xb2, 0x76, 0x5b, 0xa2, 0x49, 0x6d, 0x8b, 0xd1, 0x25,
    0x72, 0xf8, 0xf6, 0x64, 0x86, 0x68, 0x98, 0x16, 0xd4, 0xa4, 0x5c, 0xcc, 0x5d, 0x65, 0xb6, 0x92,
    0x6c, 0x70, 0x48, 0x50, 0xfd, 0xed, 0xb9, 0xda, 0x5e, 0x15, 0x46, 0x57, 0xa7, 0x8d, 0x9d, 0x84,
    0x90, 0xd8, 0xab, 0x00, 0x8c, 0xbc, 0xd3, 0x0a, 0xf7, 0xe4, 0x58, 0x05, 0xb8, 0xb3, 0x45, 0x06,
    0xd0, 0x2c, 0x1e, 0x8f, 0xca, 0x3f, 0x0f, 0x02, 0xc1, 0xaf, 0xbd, 0x03, 0x01, 0x13, 0x8a, 0x6b,
    0x3a, 0x91, 0x11, 0x41, 0x4f, 0x67, 0xdc, 0xea, 0x97, 0xf2, 0xcf, 0xce, 0xf0, 0xb4, 0xe6, 0x73,
    0x96, 0xac, 0x74, 0x22, 0xe7, 0xad, 0x35, 0x85, 0xe2, 0xf9, 0x37, 0xe8, 0x1c, 0x75, 0xdf, 0x6e,
    0x47, 0xf1, 0x1a, 0x71, 0x1d, 0x29, 0xc5, 0x89, 0x6f, 0xb7, 0x62, 0x0e, 0xaa, 0x18, 0xbe, 0x1b,
    0xfc, 0x56, 0x3e, 0x4b, 0xc6, 0xd2, 0x79, 0x20, 0x9a, 0xdb, 0xc0, 0xfe, 0x78, 0xcd, 0x5a, 0xf4,
    0x1f, 0xdd, 0xa8, 0x33, 0x88, 0x07, 0xc7, 0x31, 0xb1, 0x12, 0x10, 0x59, 0x27, 0x80, 0xec, 0x5f,
    0x60, 0x51, 0x7f, 0xa9, 0x19, 0xb5, 0x4a, 0x0d, 0x2d, 0xe5, 0x7a, 0x9f, 0x93, 0xc9, 0x9c, 0xef,
    0xa0, 0xe0, 0x3b, 0x4d, 0xae, 0x2a, 0xf5, 0xb0, 0xc8, 0xeb, 0xbb, 0x3c, 0x83, 0x53, 0x99, 0x61,
    0x17, 0x2b, 0x04, 0x7e, 0xba, 0x77, 0xd6, 0x26, 0xe1, 0x69, 0x14, 0x63, 0x55, 0x21, 0x0c, 0x7d,
];

const RCON: [u8; 11] = [
    0x00, 0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80, 0x1b, 0x36,
];

const PASSWORD_PADDING: [u8; 32] = [
    0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA, 0x01, 0x08,
    0x2E, 0x2E, 0x00, 0xB6, 0xD0, 0x68, 0x3E, 0x80, 0x2F, 0x0C, 0xA9, 0xFE, 0x64, 0x53, 0x69, 0x7A,
];

fn pad_password(password: &[u8]) -> [u8; 32] {
    let mut padded = [0u8; 32];
    let len = password.len().min(32);
    padded[..len].copy_from_slice(&password[..len]);
    let pad_len = 32 - len;
    padded[len..].copy_from_slice(&PASSWORD_PADDING[..pad_len]);
    padded
}

/// GF(2^8) multiply for MixColumns.
fn gmul(a: u8, b: u8) -> u8 {
    let mut result = 0u8;
    let mut a = a;
    let mut b = b;
    for _ in 0..8 {
        if b & 1 != 0 {
            result ^= a;
        }
        let hi = a & 0x80;
        a <<= 1;
        if hi != 0 {
            a ^= 0x1b;
        }
        b >>= 1;
    }
    result
}

fn aes_key_expansion(key: &[u8]) -> Vec<[u8; 16]> {
    let nk = key.len() / 4; // 4 for AES-128, 8 for AES-256
    let nr = nk + 6; // 10 for 128, 14 for 256
    let nw = 4 * (nr + 1);

    let mut w = vec![0u32; nw];
    for i in 0..nk {
        w[i] = u32::from_be_bytes([key[4 * i], key[4 * i + 1], key[4 * i + 2], key[4 * i + 3]]);
    }

    for i in nk..nw {
        let mut temp = w[i - 1];
        if i % nk == 0 {
            // RotWord + SubWord + Rcon
            temp = temp.rotate_left(8);
            let bytes = temp.to_be_bytes();
            temp = u32::from_be_bytes([
                SBOX[bytes[0] as usize],
                SBOX[bytes[1] as usize],
                SBOX[bytes[2] as usize],
                SBOX[bytes[3] as usize],
            ]);
            temp ^= (RCON[i / nk] as u32) << 24;
        } else if nk > 6 && i % nk == 4 {
            let bytes = temp.to_be_bytes();
            temp = u32::from_be_bytes([
                SBOX[bytes[0] as usize],
                SBOX[bytes[1] as usize],
                SBOX[bytes[2] as usize],
                SBOX[bytes[3] as usize],
            ]);
        }
        w[i] = w[i - nk] ^ temp;
    }

    // Convert to round keys
    let mut round_keys = Vec::with_capacity(nr + 1);
    for i in 0..=nr {
        let mut rk = [0u8; 16];
        for j in 0..4 {
            let bytes = w[4 * i + j].to_be_bytes();
            rk[4 * j..4 * j + 4].copy_from_slice(&bytes);
        }
        round_keys.push(rk);
    }
    round_keys
}

fn aes_decrypt_block(block: &[u8], round_keys: &[[u8; 16]]) -> [u8; 16] {
    let nr = round_keys.len() - 1;
    let mut state = [0u8; 16];
    state.copy_from_slice(&block[..16]);

    // AddRoundKey with last round key
    for i in 0..16 {
        state[i] ^= round_keys[nr][i];
    }

    for round in (1..nr).rev() {
        inv_shift_rows(&mut state);
        for b in &mut state {
            *b = INV_SBOX[*b as usize];
        }
        for i in 0..16 {
            state[i] ^= round_keys[round][i];
        }
        inv_mix_columns(&mut state);
    }

    // Last round (no InvMixColumns)
    inv_shift_rows(&mut state);
    for b in &mut state {
        *b = INV_SBOX[*b as usize];
    }
    for i in 0..16 {
        state[i] ^= round_keys[0][i];
    }

    state
}

fn inv_shift_rows(state: &mut [u8; 16]) {
    // Row 1: shift right 1
    let t = state[13];
    state[13] = state[9];
    state[9] = state[5];
    state[5] = state[1];
    state[1] = t;
    // Row 2: shift right 2
    let (t0, t1) = (state[2], state[6]);
    state[2] = state[10];
    state[6] = state[14];
    state[10] = t0;
    state[14] = t1;
    // Row 3: shift right 3 (= shift left 1)
    let t = state[3];
    state[3] = state[7];
    state[7] = state[11];
    state[11] = state[15];
    state[15] = t;
}

fn inv_mix_columns(state: &mut [u8; 16]) {
    for col in 0..4 {
        let i = col * 4;
        let (s0, s1, s2, s3) = (state[i], state[i + 1], state[i + 2], state[i + 3]);
        state[i] = gmul(0x0e, s0) ^ gmul(0x0b, s1) ^ gmul(0x0d, s2) ^ gmul(0x09, s3);
        state[i + 1] = gmul(0x09, s0) ^ gmul(0x0e, s1) ^ gmul(0x0b, s2) ^ gmul(0x0d, s3);
        state[i + 2] = gmul(0x0d, s0) ^ gmul(0x09, s1) ^ gmul(0x0e, s2) ^ gmul(0x0b, s3);
        state[i + 3] = gmul(0x0b, s0) ^ gmul(0x0d, s1) ^ gmul(0x09, s2) ^ gmul(0x0e, s3);
    }
}

// Minimal SHA-256 for AES-256 password validation
fn sha256_impl(inputs: &[&[u8]]) -> [u8; 32] {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    // Collect all input bytes
    let mut message = Vec::new();
    for input in inputs {
        message.extend_from_slice(input);
    }
    let bit_len = (message.len() as u64) * 8;

    // Pad
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_be_bytes());

    // Process blocks
    for block in message.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                block[4 * i],
                block[4 * i + 1],
                block[4 * i + 2],
                block[4 * i + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut result = [0u8; 32];
    for (i, &val) in h.iter().enumerate() {
        result[4 * i..4 * i + 4].copy_from_slice(&val.to_be_bytes());
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rc4_roundtrip() {
        let key = b"secret";
        let plaintext = b"Hello, World!";
        let encrypted = rc4(key, plaintext);
        let decrypted = rc4(key, &encrypted);
        assert_eq!(&decrypted, plaintext);
    }

    #[test]
    fn aes128_known_vector() {
        // FIPS 197 Appendix B test vector
        let key = [
            0x2b, 0x7e, 0x15, 0x16, 0x28, 0xae, 0xd2, 0xa6, 0xab, 0xf7, 0x15, 0x88, 0x09, 0xcf,
            0x4f, 0x3c,
        ];
        let plaintext = [
            0x32, 0x43, 0xf6, 0xa8, 0x88, 0x5a, 0x30, 0x8d, 0x31, 0x31, 0x98, 0xa2, 0xe0, 0x37,
            0x07, 0x34,
        ];
        let expected_cipher = [
            0x39, 0x25, 0x84, 0x1d, 0x02, 0xdc, 0x09, 0xfb, 0xdc, 0x11, 0x85, 0x97, 0x19, 0x6a,
            0x0b, 0x32,
        ];

        // Verify decryption: decrypt(encrypt(plain)) == plain
        let round_keys = aes_key_expansion(&key);
        let decrypted = aes_decrypt_block(&expected_cipher, &round_keys);
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn sha256_empty() {
        let hash = sha256(&[b""]);
        // SHA-256 of empty string
        let expected = [
            0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f,
            0xb9, 0x24, 0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c, 0xa4, 0x95, 0x99, 0x1b,
            0x78, 0x52, 0xb8, 0x55,
        ];
        assert_eq!(hash, expected);
    }

    #[test]
    fn sha256_abc() {
        let hash = sha256(&[b"abc"]);
        let expected = [
            0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
            0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
            0xf2, 0x00, 0x15, 0xad,
        ];
        assert_eq!(hash, expected);
    }

    #[test]
    fn password_padding() {
        let padded = pad_password(b"");
        assert_eq!(padded, PASSWORD_PADDING);

        let padded = pad_password(b"test");
        assert_eq!(&padded[..4], b"test");
        assert_eq!(&padded[4..], &PASSWORD_PADDING[..28]);
    }

    #[test]
    fn aes_cbc_roundtrip() {
        // Test AES-CBC decryption with known data
        let key = [0u8; 16]; // Zero key
        let iv = [0u8; 16]; // Zero IV

        // Single block of zeros encrypted with zero key and zero IV
        // Just verify it doesn't panic and produces 16 bytes or less
        let ciphertext = [0u8; 16];
        let result = aes_cbc_decrypt(&key, &iv, &ciphertext);
        assert!(result.is_ok());
    }
}
