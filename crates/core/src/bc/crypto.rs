use aes::{
    cipher::{AsyncStreamCipher, BlockEncrypt, KeyInit, KeyIvInit},
    Aes128,
};
use cfb_mode::{Decryptor, Encryptor};

type Aes128CfbEnc = Encryptor<Aes128>;
type Aes128CfbDec = Decryptor<Aes128>;

/// Decrypt with explicit CFB state (iv, num) and return updated state.
/// Not used for replay: SDK resets num=0 every packet (per-packet fresh IV only).
/// num is in 0..16 (bytes already consumed from current keystream block).
pub fn decrypt_cfb_with_state(
    key: &[u8; 16],
    iv: &mut [u8; 16],
    num: &mut u8,
    ciphertext: &[u8],
) -> Vec<u8> {
    use aes::cipher::generic_array::GenericArray;
    use aes::cipher::typenum::U16;
    let cipher = Aes128::new_from_slice(key).expect("key length");
    let mut out = Vec::with_capacity(ciphertext.len());
    let mut i = 0;
    while i < ciphertext.len() {
        let mut block: GenericArray<u8, U16> = GenericArray::clone_from_slice(iv);
        cipher.encrypt_block(&mut block);
        let keystream = block.as_slice();
        let n = (16 - (*num as usize)).min(ciphertext.len() - i);
        for j in 0..n {
            out.push(ciphertext[i + j] ^ keystream[(*num as usize) + j]);
        }
        // CFB feedback: iv is the last 16 bytes of ciphertext; shift and append each new ct byte
        for k in 0..n {
            iv[0] = iv[1];
            iv[1] = iv[2];
            iv[2] = iv[3];
            iv[3] = iv[4];
            iv[4] = iv[5];
            iv[5] = iv[6];
            iv[6] = iv[7];
            iv[7] = iv[8];
            iv[8] = iv[9];
            iv[9] = iv[10];
            iv[10] = iv[11];
            iv[11] = iv[12];
            iv[12] = iv[13];
            iv[13] = iv[14];
            iv[14] = iv[15];
            iv[15] = ciphertext[i + k];
        }
        i += n;
        *num = (*num as usize + n) as u8 % 16;
    }
    out
}

const XML_KEY: [u8; 8] = [0x1F, 0x2D, 0x3C, 0x4B, 0x5A, 0x69, 0x78, 0xFF];
/// Default IV when no custom IV is set (SDK: BaichuanEncryptor uses this when *(this+0x1c) != 0x10).
const IV: [u8; 16] = *b"0123456789abcdef";

/// These are the encyption modes supported by the camera
///
/// The mode is negotiated during login
#[derive(Debug, Clone)]
pub enum EncryptionProtocol {
    /// Older camera use no encryption
    Unencrypted,
    /// Camera/Firmwares before 2021 use BCEncrypt which is a simple XOr
    BCEncrypt,
    /// Latest cameras/firmwares use Aes with the key derived from
    /// the camera's password and the negotiated NONCE
    Aes {
        /// AES-128 key (16 bytes)
        key: [u8; 16],
        /// Master IV (16 bytes) - reset to this value for each packet
        iv: [u8; 16],
    },
    /// Same as Aes but the media stream is also encrypted and not just
    /// the control commands
    FullAes {
        /// AES-128 key (16 bytes)
        key: [u8; 16],
        /// Master IV (16 bytes) - reset to this value for each packet
        iv: [u8; 16],
    },
}

impl EncryptionProtocol {
    /// Helper to make unencrypted
    pub fn unencrypted() -> Self {
        EncryptionProtocol::Unencrypted
    }
    /// Helper to make bcencrypted
    pub fn bcencrypt() -> Self {
        EncryptionProtocol::BCEncrypt
    }
    /// Helper to make aes
    pub fn aes(key: [u8; 16]) -> Self {
        EncryptionProtocol::Aes {
            key,
            iv: IV,
        }
    }
    /// Helper to make full aes
    pub fn full_aes(key: [u8; 16]) -> Self {
        EncryptionProtocol::FullAes {
            key,
            iv: IV,
        }
    }

    /// Decrypt the data, offset comes from the header of the packet
    pub fn decrypt(&self, offset: u32, buf: &[u8]) -> Vec<u8> {
        match self {
            EncryptionProtocol::Unencrypted => buf.to_vec(),
            EncryptionProtocol::BCEncrypt => {
                let key_iter = XML_KEY.iter().cycle().skip(offset as usize % 8);
                key_iter
                    .zip(buf)
                    .map(|(key, i)| *i ^ key ^ (offset as u8))
                    .collect()
            }
            EncryptionProtocol::Aes { key, iv } | EncryptionProtocol::FullAes { key, iv } => {
                // AES decryption
                // CRITICAL: Based on Ghidra analysis, the app resets CFB state per packet.
                // Each packet starts fresh with Master IV and num=0.
                // We create a new decryptor for each packet to match this behavior.
                // CFB state is still preserved WITHIN a packet (for multi-block packets).
                let decryptor = Aes128CfbDec::new(key.as_slice().into(), iv.as_slice().into());
                let mut decrypted = buf.to_vec();
                decryptor.decrypt(&mut decrypted);
                decrypted
            }
        }
    }

    /// Decrypt with an explicit IV (e.g. per-packet IV from first 16 bytes of payload).
    /// Only supported for Aes/FullAes; returns None for other protocols.
    pub fn decrypt_with_iv(&self, packet_iv: &[u8; 16], buf: &[u8]) -> Option<Vec<u8>> {
        match self {
            EncryptionProtocol::Aes { key, .. } | EncryptionProtocol::FullAes { key, .. } => {
                let decryptor = Aes128CfbDec::new(key.as_slice().into(), packet_iv.as_slice().into());
                let mut decrypted = buf.to_vec();
                decryptor.decrypt(&mut decrypted);
                Some(decrypted)
            }
            _ => None,
        }
    }

    /// Encrypt the data, offset comes from the header of the packet
    pub fn encrypt(&self, offset: u32, buf: &[u8]) -> Vec<u8> {
        match self {
            EncryptionProtocol::Unencrypted => {
                // Encrypt is the same as decrypt
                self.decrypt(offset, buf)
            }
            EncryptionProtocol::BCEncrypt => {
                // Encrypt is the same as decrypt
                self.decrypt(offset, buf)
            }
            EncryptionProtocol::Aes { key, iv } | EncryptionProtocol::FullAes { key, iv } => {
                // AES encryption
                // CRITICAL: Based on Ghidra analysis, the app resets CFB state per packet.
                // Each packet starts fresh with Master IV and num=0.
                // We create a new encryptor for each packet to match this behavior.
                // CFB state is still preserved WITHIN a packet (for multi-block packets).
                let encryptor = Aes128CfbEnc::new(key.as_slice().into(), iv.as_slice().into());
                let mut encrypted = buf.to_vec();
                encryptor.encrypt(&mut encrypted);
                encrypted
            }
        }
    }
}

#[test]
fn test_xml_crypto() {
    let sample = include_bytes!("samples/xml_crypto_sample1.bin");
    let should_be = include_bytes!("samples/xml_crypto_sample1_plaintext.bin");

    let decrypted = EncryptionProtocol::BCEncrypt.decrypt(0, &sample[..]);
    assert_eq!(decrypted, &should_be[..]);
}

#[test]
fn test_xml_crypto_roundtrip() {
    let zeros: [u8; 256] = [0; 256];

    let decrypted = EncryptionProtocol::BCEncrypt.encrypt(0, &zeros[..]);
    let encrypted = EncryptionProtocol::BCEncrypt.decrypt(0, &decrypted[..]);
    assert_eq!(encrypted, &zeros[..]);
}

#[test]
fn test_aes_cfb_per_packet_reset() {
    // Test that CFB state resets per packet (matches app behavior from Ghidra analysis)
    // Each packet should decrypt independently, starting fresh with Master IV
    let key = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08,
               0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10];
    
    let encryptor = EncryptionProtocol::aes(key);
    let decryptor = EncryptionProtocol::aes(key);

    // Encrypt multiple packets sequentially (each starts fresh)
    let packet1 = b"Packet 1 data";
    let packet2 = b"Packet 2 data";
    let packet3 = b"Packet 3 data";

    let enc1 = encryptor.encrypt(0, packet1);
    let enc2 = encryptor.encrypt(0, packet2);
    let enc3 = encryptor.encrypt(0, packet3);

    // Decrypt packets sequentially (each starts fresh)
    let dec1 = decryptor.decrypt(0, &enc1);
    let dec2 = decryptor.decrypt(0, &enc2);
    let dec3 = decryptor.decrypt(0, &enc3);

    // Verify all packets decrypt correctly (each packet is independent)
    assert_eq!(dec1, packet1, "Packet 1 should decrypt correctly with per-packet reset");
    assert_eq!(dec2, packet2, "Packet 2 should decrypt correctly with per-packet reset");
    assert_eq!(dec3, packet3, "Packet 3 should decrypt correctly with per-packet reset");
}

#[test]
fn test_aes_cfb_within_packet() {
    // Test that CFB state is preserved WITHIN a packet (for multi-block packets)
    // A single large packet should decrypt correctly, even though packets are independent
    let key = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08,
               0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10];
    
    let encryptor = EncryptionProtocol::aes(key);
    let decryptor = EncryptionProtocol::aes(key);

    // Large packet (multiple blocks - ~62 blocks for 1000 bytes)
    let large_packet = vec![0xAA; 1000];

    let encrypted = encryptor.encrypt(0, &large_packet);
    let decrypted = decryptor.decrypt(0, &encrypted);

    assert_eq!(decrypted, large_packet, "Large packet should decrypt correctly with CFB state preserved within packet");
}

#[test]
fn test_full_aes_cfb_per_packet_reset() {
    // Test FullAes variant - per-packet reset behavior
    let key = [0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80,
               0x90, 0xa0, 0xb0, 0xc0, 0xd0, 0xe0, 0xf0, 0x00];
    
    let encryptor = EncryptionProtocol::full_aes(key);
    let decryptor = EncryptionProtocol::full_aes(key);

    let packet1 = b"FullAes packet 1";
    let packet2 = b"FullAes packet 2";

    let enc1 = encryptor.encrypt(0, packet1);
    let enc2 = encryptor.encrypt(0, packet2);

    let dec1 = decryptor.decrypt(0, &enc1);
    let dec2 = decryptor.decrypt(0, &enc2);

    assert_eq!(dec1, packet1, "FullAes packet 1 should decrypt correctly with per-packet reset");
    assert_eq!(dec2, packet2, "FullAes packet 2 should decrypt correctly with per-packet reset");
}
