# BcMedia replay format

This document describes the **BcMedia** binary format used for Reolink/Baichuan camera replay (SD card playback) and live streaming. It is a proprietary framed format; Neolink implements the only known open decoder.

---

## 1. Overview

- **Name**: BcMedia (Baichuan Media).
- **Use**: Livestream and replay video/audio over the Baichuan TCP protocol (Reolink cameras, Swann, and other OEMs using the same SDK).
- **Byte order**: Little-endian.
- **Framing**: Each logical “packet” starts with a 4-byte magic (uint32 LE); the parser then reads type-specific header and payload.

Replay over the wire can appear in two forms:

1. **BcMedia-framed stream** — Same as livestream: sequence of BcMedia packets (Info, I-frame, P-frame, AAC, ADPCM). No file container (no `ftyp`).
2. **Raw container** — Some cameras send a 32-byte replay header then raw MP4 (ISO Base Media: `ftyp` at offset 4). In that case the payload is standard MP4 and can be fed to FFmpeg directly after skipping the 32-byte header.

This document focuses on the **BcMedia** format itself. Transport (32-byte header, MSG 5/8, end codes) is summarized in §5.

---

## 2. Binary layout: packet types and magic

All packets start with a **4-byte magic** (uint32 LE). Valid magics:

| Magic (hex) | ASCII (LE) | Type        | Description                    |
|-------------|------------|-------------|--------------------------------|
| `0x31303031`| "1001"     | Info V1     | Stream info (legacy)           |
| `0x32303031`| "1002"     | Info V2     | Stream info (current)         |
| `0x63643030`..`0x63643039` | "cd00".."cd09" | I-frame | H.264/H.265 keyframe  |
| `0x63643130`..`0x63643139` | "cd10".."cd19" | P-frame | H.264/H.265 delta frame |
| `0x62773530`| "bw50"     | AAC         | AAC audio                      |
| `0x62773130`| "bw10"     | ADPCM       | ADPCM audio (DVI-4, 8 kHz)     |

- **"cd"** (video): I- and P-frame magics; the low nibble can encode NVR channel (0–9).  
- **"bw"** (audio): Both audio types use this prefix; it may mean mono / single-channel (e.g. “black & white” for one channel). The Android app uses **"wb"** for audio in a different 7-byte framing (see §7 References).  
- **No other magics**: The Neolink parser and all observed streams only use the above. There is no other BcMedia 4-byte magic in the codebase or in pcaps. (A separate, non-BcMedia format in the Android app uses 7-byte tags: `dcH26` for video, `wb` for audio; see `notes/ANDROID_REPLAY_DOWNLOAD_FLOW.md`.)

---

## 3. Packet structures (after magic)

### 3.1 Info V1 (`0x31303031`)

| Offset | Size | Field           | Description                    |
|--------|------|-----------------|--------------------------------|
| 0      | 4    | header_size     | Must be 32                     |
| 4      | 4    | video_width     | Width in pixels                |
| 8      | 4    | video_height    | Height in pixels               |
| 12     | 1    | unknown         | Reserved                        |
| 13     | 1    | fps             | FPS (or index on older cams)   |
| 14     | 1    | start_year      | Start time (year byte, e.g. 121 = 2021) |
| 15..20 | 6    | start_month..start_seconds | Start date/time |
| 21..26 | 6    | end_*           | End date/time                  |
| 27     | 2    | unknown         | Reserved                        |

**Total header**: 4 (magic) + 32 = 36 bytes.

### 3.2 Info V2 (`0x32303031`)

Same layout as Info V1 (header_size 32, width, height, fps, start/end date-time). Total header 36 bytes.

### 3.3 I-frame (`cd00`..`cd09`)

| Offset | Size | Field                  | Description                    |
|--------|------|------------------------|--------------------------------|
| 0      | 4    | video_type             | "H264" or "H265" (ASCII)       |
| 4      | 4    | payload_size           | Length of NAL data following  |
| 8      | 4    | additional_header_size | Extra header bytes after fixed fields |
| 12     | 4    | microseconds           | Timestamp (µs)                 |
| 16     | 4    | unknown                | Reserved                       |
| 20     | 0..  | optional time          | If additional_header_size ≥ 4: POSIX time (uint32) |
| 20+    | (rest)| additional_header      | Remaining extra header        |
| —      | payload_size | data   | Raw NAL (H.264/HEVC)   |
| —      | 0–7  | padding                | Pad to 8-byte boundary        |

Padding: `pad = (8 - (payload_size % 8)) % 8` bytes of zeros after the payload.

### 3.4 P-frame (`cd10`..`cd19`)

Same as I-frame but no optional `time` field in practice (additional_header_size often 0):

- video_type (4), payload_size (4), additional_header_size (4), microseconds (4), unknown (4), additional_header (additional_header_size), data (payload_size), padding to 8 bytes.

### 3.5 AAC (`0x62773530`)

| Offset | Size | Field          | Description        |
|--------|------|----------------|--------------------|
| 0      | 2    | payload_size   | Length of AAC data |
| 2      | 2    | payload_size_b | Duplicate          |
| 4      | payload_size | data   | Raw ADTS AAC     |
| —      | 0–7  | padding        | Pad to 8 bytes     |

### 3.6 ADPCM (`0x62773130`)

| Offset | Size | Field    | Description                          |
|--------|------|----------|--------------------------------------|
| 0      | 2    | payload_size | Total payload (includes sub-header) |
| 2      | 2    | payload_size_b | Duplicate                         |
| 4      | 2    | magic    | `0x0100` (MAGIC_HEADER_BCMEDIA_ADPCM_DATA) |
| 6      | 2    | half_block_size | Block size related (camera-dependent) |
| 8      | (payload_size - 4) | data | 4-byte predictor state + DVI-4 ADPCM block |
| —      | 0–7  | padding  | Pad to 8 bytes                       |

Sample rate is 8000 Hz. Block size for duration: `block_size = data.len() - 4`; duration µs = `block_size * 2 * 1_000_000 / 8000`.

---

## 4. Stream order and decoding

- **Start**: Stream typically begins with **Info V1** or **Info V2**, then I-frames and P-frames (and optionally AAC/ADPCM).
- **Parsing**: Read 4-byte magic; dispatch to the correct parser; consume header + payload + padding; repeat. If the stream is corrupted, a non-strict decoder may skip bytes until the next known magic.
- **Video**: I-frame and P-frame `data` are raw NAL units (H.264 or H.265). They may already include Annex B start codes (`0x00 0x00 0x01` or `0x00 0x00 0x00 0x01`); if not, prepend `0x00 0x00 0x00 0x01` for Annex B. FFmpeg can then read `-f h264` or `-f hevc` and mux to MP4.
- **Receive-only sentinels** (not on wire): Neolink uses `RawReplayChunk` for raw container bytes and `StreamEnd` when the camera signals end of file (response 300/331).

Reference implementation: `crates/core/src/bcmedia/` (model.rs, de.rs, ser.rs, codex.rs).

---

## 5. Replay transport (wire)

Replay is carried over Baichuan TCP messages (e.g. MSG 5, MSG 8, or desktop 0x17d):

1. **32-byte replay header**: In the **official app** (Ghidra: BaichuanReplayer::handleBinaryDataResponse, Android 0x0034b22c; Desktop FUN_1801768c0), the first packet is treated as **metadata only** (not written to the stream) **only when** **msg_id == 5**, **response_code == 200**, and **body length == 32**. For **0x17d** the app does *not* skip: it writes that first 32-byte body. Neolink skips the first 32 bytes of the accumulated buffer when it does not start with `ftyp` (so BcMedia/MP4 logic sees the real start); for strict app parity you would skip only when the replay was started with MSG 5. Optional file size may be at +0x10 and +0x18 (uint64 LE) in the 32-byte header.
2. **Response codes**: **200** = accept (replay start); **300** = end (by-name); **331** = end (by-time); **400** = reject. Streaming data packets may use other response codes; the app only writes when `header.response_code == replayer.expected_response_code` (e.g. Android this+0x1c).
3. **Following packets**: Either BcMedia-framed data, raw MP4 chunks, or (E1) **Extension XML + binary** blocks — see `notes/REPLAY_BIN_ANALYSIS.md`. If the first payload after the 32-byte header has `ftyp` at bytes 4–8, the stream is treated as raw MP4; otherwise BcMedia or E1 format.
4. **End of stream**: Camera sends **300** or **331**. Some cameras never send them; Neolink can stop when received payload size reaches the expected size from the file list or 32-byte header.
5. **Decrypt boundary (Ghidra):** Replay binary is decrypted **before** it reaches the replayer. In Android `libBCSDKWrapper.so`, `BaichuanDevice::handleResponseV20` @ 0x002bdc60: (a) decrypts the **extension** (if encrypted) via `BaichuanEncryptor::decrypt`, (b) parses `Net_get_query_from_xml`, (c) decrypts the **body** in place (or only the region given by E1 `encryptPos`/`encryptLen` when present), (d) then calls `handleBinaryDataResponse` or `BCNetSessionQueue::setResponseData`. So BcMedia/MP4/E1 payload formats in docs refer to bytes **after** this decrypt step; on wire they are encrypted (XOR or AES per login). See `notes/baichuan-bcsdk-reverse-engineering.md` (reolink) and `notes/REPLAY_SCHEMA_PLAN.md`.

See `crates/core/src/bc_protocol/replay.rs` and `dissector/PCAP_ANALYSIS.md` for message IDs and reassembly.

---

## 6. Third-party support

### 6.1 FFmpeg

**BcMedia is not implemented in FFmpeg.** There is no demuxer or format name for “BcMedia” or “Baichuan” in libavformat. The official formats list (e.g. https://ffmpeg.org/ffmpeg-formats.html) does not include it.

**Practical use with FFmpeg:**

- **BcMedia replay**: Decode BcMedia in Neolink (or similar), extract H.264/HEVC NALs, write Annex B (e.g. `.h264`), then:
  ```bash
  ffmpeg -y -f h264 -i out.h264 -c copy out.mp4
  ```
  (or `-f hevc` for H.265.)
- **Raw MP4 replay**: After skipping the 32-byte replay header, if the rest is MP4, feed it to FFmpeg as a normal file or pipe; no special demuxer needed.

So FFmpeg is used **after** converting BcMedia to raw H.264/HEVC (or when the camera sends raw MP4).

### 6.2 go2rtc

**go2rtc does not implement the BcMedia or Reolink replay format.** It supports RTSP, WebRTC, and various camera brands for **live** streams; Reolink-related issues in the tracker are about RTSP/HTTP (e.g. 400 Bad Request, connection), not about BcMedia or SD card replay.

Replay from Reolink/Baichuan (MSG 5/8, BcMedia or raw MP4) is not a go2rtc feature; it would require a custom source that speaks the Baichuan protocol and then either:

- parses BcMedia and exposes NALs (e.g. as H.264 stream), or  
- forwards raw MP4 when the camera sends it.

### 6.3 Other libraries

No other well-known open-source projects (GStreamer, VLC demuxers, etc.) were found that implement BcMedia. The only open implementation documented here is **Neolink** (`neolink_core::bcmedia` and replay in `src/replay/`).

---

## 6.4 When does the replay stream end?

We can stop in three ways:

1. **Camera end codes** — The camera sends a message with `response_code == 300` (replay by-name) or `331` (download-by-time). The core then emits `BcMedia::StreamEnd` and the app exits with “camera signalled end of file”. Some cameras (e.g. E1) may **not** send 300 before we hit a timeout.
2. **Size-based** — When we have an expected file size (from the file list or from the 32-byte replay header), the core stops when `total_binary_bytes - 32 >= expected_payload_size` and sends `StreamEnd`. So we do **not** rely only on time if size is known.
3. **Duration / timeout** — If the app has a duration (e.g. from the file list, “31 s”), it runs a timer. When the timer fires we break with “31s duration reached” and send MSG 7 (replay stop). So in that case we are **timing out**, not necessarily receiving 300 or hitting expected size.

So: we **do** know stream end when the camera sends 300/331 or when we receive the expected byte count; otherwise we stop when the duration timer fires.

---

## 6.5 E1 replay dump inspection (`out.replay.bin`)

When assembly fails, the app writes the raw reassembled stream to `out.replay.bin`. Neolink now **strips the E1 envelope** per packet (32-byte prefix + Extension XML, or XML-only when the body starts with `<?xml`) and concatenates only the **payload** bytes (see `strip_e1_replay_envelope` in `replay.rs`). So the dump is:

- **First 32 bytes**: replay header (first packet).
- **Remaining bytes**: concatenation of payloads from subsequent packets (after stripping 32+XML or XML).

**E1 two-stage decryption**: Some E1 cameras do not send `<encryptPos>`/`<encryptLen>` in the Extension. The wire payload is then `[encrypted extension][encrypted media]`. Decrypting the whole payload in one go yields valid XML at the start but **ciphertext after `</Extension>\n`** (CFB state is wrong for the rest). Neolink therefore uses a **two-stage** decrypt when FullAes + binary and no encryptLen: decrypt once to find `</Extension>\n`, then decrypt `payload[0..ext_len]` and `payload[ext_len..]` **separately** (IV reset for each call), and concatenate. So the bytes after the XML become plaintext (e.g. ftyp MP4).

If the dump **still shows uniform high-entropy binary** (no `ftyp`, no `mdat`, no XML) after the 32-byte header, then the payload we are receiving is either **still encrypted** or **decrypted with the wrong parameters**. That points to E1 **payload decryption** being wrong for replay, for example:

- **Per-packet IV**: the camera may re‑key or advance an IV (e.g. counter) per packet; the session cipher might need to be reset or updated per E1 packet.
- **Wrong region**: `encrypt_pos` / `encrypt_len` from the Extension might be interpreted differently (e.g. offsets into a different buffer, or lengths in another unit).
- **Replay-specific key**: replay might use a different key or nonce than the rest of the session.

**Debug logging**: With `RUST_LOG=neolink_core=debug`, Neolink logs E1 decrypt and strip details: in **de.rs** (`E1 decrypt: msg_id=... encryptPos=... encryptLen=... processed_first_64=...`) and in **replay.rs** for the first 3 packets (`E1 strip: pkt=... data_first_64=... payload_first_64=...`). Example: `RUST_LOG=neolink_core=debug ./target/release/neolink replay koty play --name 0120260204150221 --duration 5 --config=neolink.toml 2>&1 | head -200`.

**Next steps**: capture a replay session (e.g. with the official app) in a pcap and compare the same packet (same offset) with Neolink’s decrypted output; or enable debug logs in `de.rs` to print the first few bytes of the decrypted region and of the final Binary payload for one E1 packet.

---

## 6.6 E1 replay start (file name format)

E1 cameras can **reject** MSG 5/8 (400) or 0x17d (405) when the replay file name does not match the format they expect. Observed behaviour:

- **Rejected**: `01_20260204120000` (underscore, readable date) → 400 for MSG 5 and 8, 405 for 0x17d.
- **Accepted**: `0120260204150221` (no underscore; `01` + `YYYYMMDDHHMMSS`) → MSG 5 returns 200 and streaming starts.

Use the exact file names listed by `neolink replay <camera> files --date YYYY-MM-DD` (e.g. `0120260204150221`). If replay start returns 400/405, try a different file from the list; the naming may vary by firmware.

---

## 6.7 E1 replay decoder (Python) and pcaps in repo

Pcaps from replay sessions are in the repo root (e.g. `PCAPdroid_08_Feb_04_24_56.pcap`, `PCAPdroid_06_Feb_13_42_19.pcap`). Use them to test the Python E1 decoder and compare with Neolink.

**How to run the E1 replay decoder** (from repo root, with [uv](https://docs.astral.sh/uv/)):

```bash
# Password from env or neolink.toml in repo root
uv run python scripts/e1_replay_decoder.py PCAPdroid_08_Feb_04_24_56.pcap e1_decoded.bin

# Or with explicit password (same as in neolink.toml for the camera)
uv run python scripts/e1_replay_decoder.py --password 'YourCameraPassword' PCAPdroid_08_Feb_04_24_56.pcap e1_decoded.bin

# If the pcap has multiple TCP streams to the camera, try --stream 2 etc.
uv run python scripts/e1_replay_decoder.py --stream 2 --password '...' PCAPdroid_08_Feb_04_24_56.pcap out.bin
```

Output: writes decoded bytes to the given path (default `e1_replay_decoded.bin` in the pcap’s directory) and prints whether the stream starts with `ftyp`. If the decoder yields `ftyp`, align Neolink’s E1 decrypt/strip logic with the script.

---

## 7. References

- Neolink: `crates/core/src/bcmedia/` (model, de, ser, codex), `crates/core/src/bc_protocol/replay.rs`, `src/replay/mod.rs`
- Notes: `notes/REPLAY_VIDEO_FORMAT.md` (wire format variants), `notes/REPLAY_RE_ANALYSIS.md`, `dissector/PCAP_ANALYSIS.md`
- Dissector: `dissector/baichuan.lua` (replay export skips 32-byte header, concatenates until 300/331)
- Script: `scripts/extract_replay_from_pcap_app.py` (BcMedia parse → NAL → Annex B → FFmpeg mux)
