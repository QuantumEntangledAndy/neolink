use super::model::*;
use crate::Error;
use bytes::{Buf, BytesMut};
use log::*;
use nom::{
    bytes::streaming::take, combinator::*, error::context as error_context, number::streaming::*,
    sequence::*, Parser,
};

type IResult<I, O, E = nom::error::VerboseError<I>> = Result<(I, O), nom::Err<E>>;

impl Bc {
    /// Returns Ok(deserialized data, the amount of data consumed)
    /// Can then use this as the amount that should be remove from a buffer
    pub(crate) fn deserialize(context: &BcContext, buf: &mut BytesMut) -> Result<Bc, Error> {
        let parser = BcParser { context };
        let (result, amount) = match consumed(parser)(buf) {
            Ok((_, (parsed_buff, result))) => Ok((result, parsed_buff.len())),
            Err(e) => Err(Error::from(e)),
        }?;

        buf.advance(amount);
        Ok(result)
    }
}

struct BcParser<'a> {
    context: &'a BcContext,
}

impl<'a> Parser<&'a [u8], Bc, nom::error::VerboseError<&'a [u8]>> for BcParser<'a> {
    fn parse(&mut self, buf: &'a [u8]) -> IResult<&'a [u8], Bc> {
        bc_msg(self.context, buf)
    }
}

/// E1/v2 cameras always send 24-byte headers (20-byte standard + 4-byte payload_offset) for
/// replay binary (MSG 5/8), even when the class field doesn't match the values that trigger
/// `has_payload_offset`. The SDK determines header size from protocol version (device-level),
/// not from class (per-packet). When payload_offset is missing from bc_header, read the 4 bytes
/// here and set it — this gives bc_modern_msg the correct ext_len to separate Extension XML
/// from binary payload, enabling encryptPos/encryptLen extraction.
fn bc_msg<'a>(context: &BcContext, buf: &'a [u8]) -> IResult<&'a [u8], Bc> {
    let (buf, header) = bc_header(buf)?;
    let (buf, header) = if (header.msg_id == MSG_ID_REPLAY_START
        || header.msg_id == MSG_ID_REPLAY_START_ALT)
        && header.payload_offset.is_none()
    {
        let (buf, ext_len) = le_u32(buf)?;
        let mut header = header;
        header.payload_offset = Some(ext_len);
        (buf, header)
    } else {
        (buf, header)
    };
    let (buf, body) = bc_body(context, &header, buf)?;

    let bc = Bc {
        meta: header.to_meta(),
        body,
    };

    Ok((buf, bc))
}

fn bc_body<'a>(context: &BcContext, header: &BcHeader, buf: &'a [u8]) -> IResult<&'a [u8], BcBody> {
    if header.is_modern() {
        let (buf, body) = bc_modern_msg(context, header, buf)?;
        Ok((buf, BcBody::ModernMsg(body)))
    } else {
        let (buf, body) = match header.msg_id {
            MSG_ID_LOGIN => bc_legacy_login_msg(buf)?,
            _ => (buf, LegacyMsg::UnknownMsg),
        };
        Ok((buf, BcBody::LegacyMsg(body)))
    }
}

fn hex32<'a>() -> impl FnMut(&'a [u8]) -> IResult<&'a [u8], String> {
    map_res(take(32usize), |slice: &'a [u8]| {
        String::from_utf8(slice.to_vec())
    })
}

fn bc_legacy_login_msg(buf: &'_ [u8]) -> IResult<&'_ [u8], LegacyMsg> {
    let (buf, username) = hex32()(buf)?;
    let (buf, password) = hex32()(buf)?;

    Ok((buf, LegacyMsg::LoginMsg { username, password }))
}

fn bc_modern_msg<'a>(
    context: &BcContext,
    header: &BcHeader,
    buf: &'a [u8],
) -> IResult<&'a [u8], ModernMsg> {
    use nom::{
        error::{ContextError, ErrorKind, ParseError},
        Err,
    };

    fn make_error<I, E>(input: I, ctx: &'static str, kind: ErrorKind) -> E
    where
        I: std::marker::Copy,
        E: ParseError<I> + ContextError<I>,
    {
        E::add_context(input, ctx, E::from_error_kind(input, kind))
    }

    // If missing payload_offset treat all as payload
    let ext_len = header.payload_offset.unwrap_or_default();

    let (buf, ext_buf) = take(ext_len)(buf)?;
    let payload_len = header.body_len - ext_len;
    let (buf, payload_buf) = take(payload_len)(buf)?;

    let decrypted;
    let processed_ext_buf = match context.get_encrypted() {
        EncryptionProtocol::Unencrypted => ext_buf,
        encryption_protocol => {
            decrypted = encryption_protocol.decrypt(header.channel_id as u32, ext_buf);
            &decrypted
        }
    };

    let mut in_binary = false;
    // E1/replay: when set with encrypt_region_len, only this region of the payload is decrypted (Ghidra: netc_query_param_t encryptPos/encryptLen).
    let mut encrypt_region_start: Option<u32> = None;
    let mut encrypt_region_len: Option<u32> = None;
    // Now we'll take the buffer that Nom gave a ref to and parse it.
    let extension = if ext_len > 0 {
        if context.debug {
            println!(
                "Extension Txt: {:?}",
                String::from_utf8(processed_ext_buf.to_vec()).unwrap_or("Not Text".to_string())
            );
        }
        // Apply the XML parse function, but throw away the reference to decrypted in the Ok and
        // Err case. This error-error-error thing is the same idiom Nom uses internally.
        let parsed = Extension::try_parse(processed_ext_buf).map_err(|_| {
            log::error!("Extension buffer: {:?}", processed_ext_buf);
            Err::Error(make_error(
                buf,
                "Unable to parse Extension XML",
                ErrorKind::MapRes,
            ))
        })?;
        if let Extension {
            binary_data: Some(1),
            encrypt_pos,
            encrypt_len,
            ..
        } = parsed
        {
            // In binary so tell the current context that we need to treat the payload as binary
            in_binary = true;
            // So continuation packets (same msg_num, no Extension) are also treated as binary and decrypted
            context.binary_on_shared(header.msg_num);
            encrypt_region_start = encrypt_pos;
            encrypt_region_len = encrypt_len;
            log::debug!(
                "E1 Extension: msg_id={} msg_num={} ext_len={} encryptPos={:?} encryptLen={:?}",
                header.msg_id,
                header.msg_num,
                ext_len,
                encrypt_pos,
                encrypt_len
            );
        }
        Some(parsed)
    } else {
        None
    };

    // Now to handle the payload block
    // This block can either be xml or binary depending on what the message expects.
    // For our purposes we use try_parse and if all xml based parsers fail we treat
    // As binary
    let payload;
    if payload_len > 0 {
        // E1 replay: camera may send extension+media in one payload with no separate extension block (ext_len=0).
        // Treat as binary so two-stage decrypt (extension then media, IV reset) is attempted.
        if ext_len == 0
            && (header.msg_id == MSG_ID_REPLAY_START || header.msg_id == MSG_ID_REPLAY_START_ALT)
        {
            in_binary = true;
        }
        // Extract remainder of message as binary, if it exists
        const UNENCRYPTED: EncryptionProtocol = EncryptionProtocol::Unencrypted;
        const BC_ENCRYPTED: EncryptionProtocol = EncryptionProtocol::BCEncrypt;
        let encryption_protocol = match header {
            BcHeader {
                msg_id: 1,
                response_code,
                ..
            } if (response_code >> 8) & 0xff == 0xdd => {
                // 0xdd means we are setting the encryption method
                // Durig login, the max encryption is BcEncrypt since
                // the nonce has not been exchanged yet
                match response_code & 0xff {
                    0x00 => &UNENCRYPTED,
                    _ => &BC_ENCRYPTED,
                }
            }
            BcHeader { msg_id: 1, .. } => {
                match &context.get_encrypted() {
                    EncryptionProtocol::Aes { .. } | EncryptionProtocol::FullAes { .. } => {
                        // During login max is BcEncrypt
                        &BC_ENCRYPTED
                    }
                    n => *n,
                }
            }
            _ => context.get_encrypted(),
        };

        // Determine if this packet needs payload decryption.
        // SDK behavior (handleResponseV20): when encryptLen is absent from the Extension,
        // it stays at init value 0xFFFFFFFF; cast to signed = -1; the check
        // `0 < (int)encryptLen` fails → decrypt is skipped → payload is plaintext.
        // This applies to BOTH:
        //   (a) continuation packets (ext_len=0, no Extension at all), AND
        //   (b) packets with Extension containing binaryData=1 but no encryptLen tag.
        // Only decrypt when encryptLen is explicitly present and > 0.
        let is_continuation_binary = ext_len == 0
            && encrypt_region_len.is_none()
            && context.in_bin_mode.borrow().contains(&(header.msg_num as u16));

        // Replay diagnostic: log ext_len and branch so we can see if camera sends extension and if we treat as continuation.
        if (header.msg_id == MSG_ID_REPLAY_START || header.msg_id == MSG_ID_REPLAY_START_ALT)
            && payload_len > 0
        {
            log::debug!(
                "E1 replay branch: msg_num={} ext_len={} payload_len={} in_binary={} encrypt_region={} is_continuation={}",
                header.msg_num,
                ext_len,
                payload_len,
                in_binary,
                encrypt_region_len.is_some(),
                is_continuation_binary
            );
        }

        let processed_payload_buf = if is_continuation_binary {
            // SDK (handleResponseV20): for binary messages (MSG 3/5/8), only bytes
            // [encryptPos, encryptPos+encryptLen) are decrypted. Continuation packets
            // have no Extension XML so encryptLen stays at init value 0 → SDK skips
            // decrypt entirely. Pass through as plaintext.
            log::debug!(
                "E1 replay: continuation packet msg_num={} len={} — no Extension, passing as plaintext (SDK: encryptLen=0)",
                header.msg_num,
                payload_buf.len()
            );
            payload_buf.to_vec()
        } else if let Some(len) = encrypt_region_len {
            // SDK: only [encryptPos, encryptPos+encryptLen) of the binary payload is decrypted (payload_buf = after extension).
            let start = encrypt_region_start.unwrap_or(0) as usize;
            let len = len as usize;
            if start <= payload_buf.len() && start.saturating_add(len) <= payload_buf.len() {
                let mut out = payload_buf.to_vec();
                let decrypted = encryption_protocol.decrypt(
                    header.channel_id as u32,
                    &payload_buf[start..start + len],
                );
                out[start..start + len].copy_from_slice(&decrypted);
                out
            } else {
                // SDK errors when encryptPos+encryptLen > payload; does NOT fallback
                // to full decrypt. Pass through as-is.
                log::warn!(
                    "E1: encryptPos({})+encryptLen({}) exceeds payload({}), skipping binary decrypt",
                    start, len, payload_buf.len()
                );
                payload_buf.to_vec()
            }
        } else if in_binary && encrypt_region_len.is_none() {
            // SDK: Extension has binaryData=1 but no encryptLen → absent encryptLen
            // defaults to 0xFFFFFFFF, (int)0xFFFFFFFF = -1, `0 < -1` fails → plaintext.
            // E1 sends large video continuation packets this way (only the first packet
            // of a BcMedia group carries explicit encryptLen for the encrypted header region).
            log::debug!(
                "E1 replay: binary packet with no encryptLen msg_num={} len={} — passing as plaintext",
                header.msg_num,
                payload_buf.len()
            );
            payload_buf.to_vec()
        } else {
            encryption_protocol.decrypt(header.channel_id as u32, payload_buf)
        };

        // E1 replay: log first binary packet result so we can verify 00dc appears (key/IV/region correct).
        if (header.msg_id == MSG_ID_REPLAY_START || header.msg_id == MSG_ID_REPLAY_START_ALT)
            && (encrypt_region_len.is_some() || in_binary)
        {
            let has_00dc = processed_payload_buf.len() >= 4
                && (processed_payload_buf[..4] == *b"00dc"
                    || (processed_payload_buf.len() > 32 && processed_payload_buf[32..36] == *b"00dc"));
            log::debug!(
                "E1 replay first/region packet: msg_num={} encrypt_region={:?} payload_len={} processed_len={} has_00dc={} first_32={:02x?}",
                header.msg_num,
                encrypt_region_len.map(|l| (encrypt_region_start.unwrap_or(0), l)),
                payload_buf.len(),
                processed_payload_buf.len(),
                has_00dc,
                &processed_payload_buf[..processed_payload_buf.len().min(32)]
            );
        }

        if context.in_bin_mode.borrow().contains(&(header.msg_num)) || in_binary {
            payload = if context.replay_raw_binary
                && (header.msg_id == MSG_ID_REPLAY_START
                    || header.msg_id == MSG_ID_REPLAY_START_ALT
                    || header.msg_id == MSG_ID_REPLAY_DESKTOP)
            {
                // Skip decryption for replay (test: some cameras send replay in plaintext).
                // Set NEOLINK_REPLAY_RAW=1 to enable.
                log::debug!("Replay: passing binary payload without decryption (NEOLINK_REPLAY_RAW)");
                Some(BcPayloads::Binary(payload_buf.to_vec()))
            } else {
                match (context.get_encrypted(), encrypt_region_len) {
                    (EncryptionProtocol::FullAes { .. }, Some(_)) => {
                        // E1 or FullAes with encryptLen: we decrypted only the region; rest is plaintext.
                        Some(BcPayloads::Binary(processed_payload_buf.to_vec()))
                    }
                    (EncryptionProtocol::Unencrypted, _) => {
                        Some(BcPayloads::Binary(payload_buf.to_vec()))
                    }
                    (EncryptionProtocol::Aes { .. }, None) => {
                        // Aes cameras encrypt control messages only; binary media streams are plaintext.
                        // Applying AES decrypt to plaintext BcMedia produces garbled output (e.g. Argus 2).
                        log::debug!(
                            "Aes (non-FullAes) camera: binary payload without explicit encryptLen — passing as plaintext (media not encrypted)"
                        );
                        Some(BcPayloads::Binary(payload_buf.to_vec()))
                    }
                    _ => {
                        // FullAes without encryptLen (plaintext passthrough), or BCEncrypt.
                        Some(BcPayloads::Binary(processed_payload_buf.to_vec()))
                    }
                }
            };
        } else {
            if context.debug {
                println!(
                    "Payload Txt: {:?}",
                    String::from_utf8(processed_payload_buf.to_vec())
                        .unwrap_or("Not Text".to_string())
                );
            }
            let xml = BcXml::try_parse(processed_payload_buf.as_slice()).map_err(|e| {
                error!("header.msg_id: {}", header.msg_id);
                error!(
                    "processed_payload_buf: {:X?}::{:?}",
                    processed_payload_buf,
                    std::str::from_utf8(&processed_payload_buf)
                );
                log::error!("e: {:?}", e);
                Err::Error(make_error(
                    buf,
                    "Unable to parse Payload XML",
                    ErrorKind::MapRes,
                ))
            })?;
            payload = Some(BcPayloads::BcXml(xml));
        }
    } else {
        payload = None;
    }

    Ok((buf, ModernMsg { extension, payload }))
}

fn bc_header(buf: &[u8]) -> IResult<&[u8], BcHeader> {
    let (buf, _magic) = error_context(
        "Magic invalid",
        verify(le_u32, |x| *x == MAGIC_HEADER || *x == MAGIC_HEADER_REV),
    )(buf)?;
    let (buf, msg_id) = error_context("MsgID missing", le_u32)(buf)?;
    let (buf, body_len) = error_context("BodyLen missing", le_u32)(buf)?;
    let (buf, channel_id) = error_context("ChannelID missing", le_u8)(buf)?;
    let (buf, stream_type) = error_context("StreamType missing", le_u8)(buf)?;
    let (buf, msg_num) = error_context("MsgNum missing", le_u16)(buf)?;
    let (buf, (response_code, class)) =
        error_context("ResponseCode missing", tuple((le_u16, le_u16)))(buf)?;

    let (buf, payload_offset) = error_context(
        "Payload Offset is missing",
        cond(has_payload_offset(class), le_u32),
    )(buf)?;

    Ok((
        buf,
        BcHeader {
            body_len,
            msg_id,
            channel_id,
            stream_type,
            msg_num,
            response_code,
            class,
            payload_offset,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bc::xml::*;
    use assert_matches::assert_matches;
    use env_logger::Env;

    fn init() {
        let _ = env_logger::Builder::from_env(Env::default().default_filter_or("info"))
            .is_test(true)
            .try_init();
    }

    #[test]
    fn test_bc_modern_login() {
        init();

        let sample = include_bytes!("samples/model_sample_modern_login.bin");

        let context = BcContext::new_with_encryption(EncryptionProtocol::BCEncrypt);

        let (buf, header) = bc_header(&sample[..]).unwrap();
        let (_, body) = bc_body(&context, &header, buf).unwrap();
        assert_eq!(header.msg_id, 1);
        assert_eq!(header.body_len, 145);
        assert_eq!(header.channel_id, 0);
        assert_eq!(header.stream_type, 0);
        assert_eq!(header.payload_offset, None);
        assert_eq!(header.response_code, 0xdd01);
        assert_eq!(header.class, 0x6614);
        match body {
            BcBody::ModernMsg(ModernMsg {
                payload:
                    Some(BcPayloads::BcXml(BcXml {
                        encryption: Some(encryption),
                        ..
                    })),
                ..
            }) => assert_eq!(encryption.nonce, "9E6D1FCB9E69846D"),
            _ => panic!(),
        }
    }

    #[test]
    // This is an 0xdd03 encryption from an Argus 2
    fn test_03_enc_login() {
        init();

        let sample = include_bytes!("samples/battery_enc.bin");

        let context = BcContext::new_with_encryption(EncryptionProtocol::BCEncrypt);

        let (buf, header) = bc_header(&sample[..]).unwrap();
        let (_, body) = bc_body(&context, &header, buf).unwrap();
        assert_eq!(header.msg_id, 1);
        assert_eq!(header.body_len, 175);
        assert_eq!(header.channel_id, 0);
        assert_eq!(header.stream_type, 0);
        assert_eq!(header.payload_offset, None);
        assert_eq!(header.response_code, 0xdd03);
        assert_eq!(header.class, 0x6614);
        match body {
            BcBody::ModernMsg(ModernMsg {
                payload:
                    Some(BcPayloads::BcXml(BcXml {
                        encryption: Some(encryption),
                        ..
                    })),
                ..
            }) => assert_eq!(encryption.nonce, "0-AhnEZyUg6eKrJFIWgXPF"),
            _ => panic!(),
        }
    }

    #[test]
    fn test_bc_legacy_login() {
        init();

        let sample = include_bytes!("samples/model_sample_legacy_login.bin");

        let context = BcContext::new_with_encryption(EncryptionProtocol::BCEncrypt);

        let (buf, header) = bc_header(&sample[..]).unwrap();
        let (_, body) = bc_body(&context, &header, buf).unwrap();
        assert_eq!(header.msg_id, 1);
        assert_eq!(header.body_len, 1836);
        assert_eq!(header.channel_id, 0);
        assert_eq!(header.stream_type, 0);
        assert_eq!(header.payload_offset, None);
        assert_eq!(header.response_code, 0xdc01);
        assert_eq!(header.class, 0x6514);
        match body {
            BcBody::LegacyMsg(LegacyMsg::LoginMsg { username, password }) => {
                assert_eq!(username, "21232F297A57A5A743894A0E4A801FC\0");
                assert_eq!(password, EMPTY_LEGACY_PASSWORD);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn test_bc_modern_login_failed() {
        init();

        let sample = include_bytes!("samples/modern_login_failed.bin");

        let context = BcContext::new_with_encryption(EncryptionProtocol::BCEncrypt);

        let (buf, header) = bc_header(&sample[..]).unwrap();
        let (_, body) = bc_body(&context, &header, buf).unwrap();
        assert_eq!(header.msg_id, 1);
        assert_eq!(header.body_len, 0);
        assert_eq!(header.channel_id, 0);
        assert_eq!(header.stream_type, 0);
        assert_eq!(header.payload_offset, Some(0x0));
        assert_eq!(header.response_code, 0x190); // 400
        assert_eq!(header.class, 0x0000);
        match body {
            BcBody::ModernMsg(ModernMsg {
                extension: None,
                payload: None,
            }) => {}
            _ => panic!(),
        }
    }

    #[test]
    fn test_bc_modern_login_success() {
        init();

        let sample = include_bytes!("samples/modern_login_success.bin");

        let context = BcContext::new_with_encryption(EncryptionProtocol::BCEncrypt);

        let (buf, header) = bc_header(&sample[..]).unwrap();
        let (_, body) = bc_body(&context, &header, buf).unwrap();
        assert_eq!(header.msg_id, 1);
        assert_eq!(header.body_len, 2949);
        assert_eq!(header.channel_id, 0);
        assert_eq!(header.stream_type, 0);
        assert_eq!(header.payload_offset, Some(0x0));
        assert_eq!(header.response_code, 0xc8); // 200
        assert_eq!(header.class, 0x0000);

        // Previously, we were not handling payload_offset == 0 (no bin offset) correctly.
        // Test that we decoded XML and no binary.
        match body {
            BcBody::ModernMsg(ModernMsg {
                extension: None,
                payload: Some(_),
            }) => {}
            _ => panic!(),
        }
    }

    #[test]
    fn test_bc_binary_mode() {
        init();

        let sample1 = include_bytes!("samples/modern_video_start1.bin");
        let sample2 = include_bytes!("samples/modern_video_start2.bin");

        let context = BcContext::new_with_encryption(EncryptionProtocol::BCEncrypt);

        let msg1 = Bc::deserialize(&context, &mut BytesMut::from(&sample1[..])).unwrap();
        match msg1.body {
            BcBody::ModernMsg(ModernMsg {
                extension:
                    Some(Extension {
                        binary_data: Some(1),
                        ..
                    }),
                payload: Some(BcPayloads::Binary(bin)),
            }) => {
                assert_eq!(bin.len(), 32);
            }
            _ => panic!(),
        }

        context.in_bin_mode.borrow_mut().insert(msg1.meta.msg_num);
        let msg2 = Bc::deserialize(&context, &mut BytesMut::from(&sample2[..])).unwrap();
        match msg2.body {
            BcBody::ModernMsg(ModernMsg {
                extension: None,
                payload: Some(BcPayloads::Binary(bin)),
            }) => {
                assert_eq!(bin.len(), 30344);
            }
            _ => panic!(),
        }
    }

    #[test]
    // B800 seems to have a different header to the E1 and swann cameras
    // the stream_type and message_num do not seem to set in the official clients
    //
    // They also have extra streams
    fn test_bc_b800_externstream() {
        init();

        let sample = include_bytes!("samples/xml_externstream_b800.bin");

        let context = BcContext::new_with_encryption(EncryptionProtocol::BCEncrypt);

        let e = Bc::deserialize(&context, &mut BytesMut::from(&sample[..]));
        assert_matches!(
            e,
            Ok(Bc {
                meta:
                    BcMeta {
                        msg_id: 3,
                        channel_id: 0x8c,
                        stream_type: 0,
                        response_code: 0,
                        msg_num: 0,
                        class: 0x6414,
                    },
                body:
                    BcBody::ModernMsg(ModernMsg {
                        extension: None,
                        payload:
                            Some(BcPayloads::BcXml(BcXml {
                                preview:
                                    Some(Preview {
                                        version,
                                        channel_id: 0,
                                        handle: 1024,
                                        stream_type,
                                    }),
                                ..
                            })),
                    }),
            }) if version == "1.1" && stream_type == Some("externStream".to_string())
        );
    }

    #[test]
    // B800 seems to have a different header to the E1 and swann cameras
    // the stream_type and message_num do not seem to set in the official clients
    //
    // They also have extra streams
    fn test_bc_b800_substream() {
        init();

        let sample = include_bytes!("samples/xml_substream_b800.bin");

        let context = BcContext::new_with_encryption(EncryptionProtocol::BCEncrypt);

        let e = Bc::deserialize(&context, &mut BytesMut::from(&sample[..]));
        assert_matches!(
            e,
            Ok(Bc {
                meta:
                    BcMeta {
                        msg_id: 3,
                        channel_id: 143,
                        stream_type: 0,
                        response_code: 0,
                        msg_num: 0,
                        class: 0x6414,
                    },
                body:
                    BcBody::ModernMsg(ModernMsg {
                        extension: None,
                        payload:
                            Some(BcPayloads::BcXml(BcXml {
                                preview:
                                    Some(Preview {
                                        version,
                                        channel_id: 0,
                                        handle: 256,
                                        stream_type,
                                    }),
                                ..
                            })),
                    }),
            }) if version == "1.1" && stream_type == Some("subStream".to_string())
        );
    }

    #[test]
    // B800 seems to have a different header to the E1 and swann cameras
    // the stream_type and message_num do not seem to set in the official clients
    //
    // They also have extra streams
    fn test_bc_b800_mainstream() {
        init();

        let sample = include_bytes!("samples/xml_mainstream_b800.bin");

        let context = BcContext::new_with_encryption(EncryptionProtocol::BCEncrypt);

        let e = Bc::deserialize(&context, &mut BytesMut::from(&sample[..]));
        assert_matches!(
            e,
            Ok(Bc {
                meta:
                    BcMeta {
                        msg_id: 3,
                        channel_id: 138,
                        stream_type: 0,
                        response_code: 0,
                        msg_num: 0,
                        class: 0x6414,
                    },
                body:
                    BcBody::ModernMsg(ModernMsg {
                        extension: None,
                        payload:
                            Some(BcPayloads::BcXml(BcXml {
                                preview:
                                    Some(Preview {
                                        version,
                                        channel_id: 0,
                                        handle: 0,
                                        stream_type,
                                    }),
                                ..
                            })),
                    }),
            }) if version == "1.1" && stream_type == Some("mainStream".to_string())
        );
    }

    #[test]
    fn test_bc_e1_mixed_replay() {
        init();
        // Use BCEncrypt (default AES-128-CFB)
        let context = BcContext::new_with_encryption(EncryptionProtocol::BCEncrypt);

        // 1. Enable in_bin_mode for msg_num=100
        let msg_num = 100u16;
        context.in_bin_mode.borrow_mut().insert(msg_num);

        // 2. Test XML Packet (Plaintext Passthrough)
        // E1 sends plaintext XML even though it's a "continuation binary" packet (no extension).
        // Our fix should detect "<?xml" and skip decryption.
        let xml_payload = b"<?xml version=\"1.0\"?><SomeData>Plaintext</SomeData>";
        let mut buf_b = Vec::new();
        // Header: Magic, MsgID=5 (Replay), BodyLen, Chan=0, Stream=0, MsgNum=100, Class=0, PayloadOffset=0
        buf_b.extend_from_slice(&0x0abcdef0u32.to_le_bytes()); // MAGIC_HEADER
        buf_b.extend_from_slice(&5u32.to_le_bytes()); // MsgID 5
        buf_b.extend_from_slice(&(xml_payload.len() as u32).to_le_bytes()); // BodyLen
        buf_b.push(0); // Chan
        buf_b.push(0); // Stream
        buf_b.extend_from_slice(&msg_num.to_le_bytes()); // MsgNum
        buf_b.extend_from_slice(&0u16.to_le_bytes()); // Response
        buf_b.extend_from_slice(&0u16.to_le_bytes()); // Class=0 → has_payload_offset=true
        buf_b.extend_from_slice(&0u32.to_le_bytes()); // PayloadOffset=0 (ext_len=0)

        buf_b.extend_from_slice(xml_payload);

        let msg_b = Bc::deserialize(&context, &mut BytesMut::from(&buf_b[..])).unwrap();
        match msg_b.body {
             BcBody::ModernMsg(ModernMsg { payload: Some(BcPayloads::Binary(data)), .. }) => {
                 // Should be Passthrough (Plaintext)
                 assert_eq!(data, xml_payload, "XML payload should be passed through plaintext");
             },
             _ => panic!("Expected Binary payload for XML packet, got {:?}", msg_b.body),
        }

        // 3. Test Binary Packet (Encrypted)
        // E1 sends encrypted binary data (e.g. video) in continuation packets.
        // Our fix should decrypt this.
        let raw_bin_payload = b"\xca\x75\xbe\x31\x81\x78\xbd\x31";
        let mut buf_c = Vec::new();
        // Header: Same as B but different payload
        buf_c.extend_from_slice(&0x0abcdef0u32.to_le_bytes());
        buf_c.extend_from_slice(&5u32.to_le_bytes());
        buf_c.extend_from_slice(&(raw_bin_payload.len() as u32).to_le_bytes());
        buf_c.push(0);
        buf_c.push(0);
        buf_c.extend_from_slice(&msg_num.to_le_bytes());
        buf_c.extend_from_slice(&0u16.to_le_bytes());
        buf_c.extend_from_slice(&0u16.to_le_bytes());
        buf_c.extend_from_slice(&0u32.to_le_bytes()); // PayloadOffset=0

        buf_c.extend_from_slice(raw_bin_payload);

        let msg_c = Bc::deserialize(&context, &mut BytesMut::from(&buf_c[..])).unwrap();
        match msg_c.body {
             BcBody::ModernMsg(ModernMsg { payload: Some(BcPayloads::Binary(data)), .. }) => {
                 // SDK: continuation binary (no Extension) has encryptLen=0 → not decrypted.
                 // Data is passed through as plaintext.
                 assert_eq!(data, raw_bin_payload, "Continuation binary should pass through as plaintext (SDK: no encryptLen)");
             },
             _ => panic!("Expected Binary payload for Binary packet, got {:?}", msg_c.body),
        }
    }
}
