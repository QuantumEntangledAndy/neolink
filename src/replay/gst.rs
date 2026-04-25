//! GStreamer pipeline to mux H.264 + optional AAC audio to MP4.
//! Supports per-frame timestamps (VFR) via appsrc PTS.
//! Requires the `gstreamer` feature.

use anyhow::{Context, Result};
use gstreamer::prelude::*;
use gstreamer::{Caps, ClockTime, MessageView, Pipeline, State};
use gstreamer_app::AppSrc;
use std::path::Path;

/// Annex B start code.
const START_CODE: &[u8] = &[0x00, 0x00, 0x00, 0x01];

/// Optional metadata to embed in the MP4 container.
#[derive(Debug, Default, Clone)]
pub struct Mp4Metadata {
    /// Camera recording type / AI detection tags (e.g. "manual,sched,md,people,vehicle,dog_cat").
    /// Stored as MP4 comment + keywords.
    pub record_type: Option<String>,
    /// Recording start time as "YYYY-MM-DD HH:MM:SS".
    pub start_time: Option<String>,
    /// Recording end time as "YYYY-MM-DD HH:MM:SS".
    pub end_time: Option<String>,
    /// Camera name / channel.
    pub camera_name: Option<String>,
}

/// Mux H.264 NALs with per-frame timestamps (+ optional AAC audio) to MP4 using GStreamer.
///
/// Pipeline:
///   video: appsrc(PTS per buffer) ! h264parse ! mp4mux ! filesink
///   audio: appsrc(PTS per buffer) ! aacparse  ! mp4mux
///
/// Each NAL in `nals` is pushed as a separate buffer with PTS derived from `timestamps_us`.
/// `timestamps_us[i]` corresponds to `nals[i]` (microseconds, from BcMedia Iframe/Pframe header).
/// If `aac_data` is non-empty, ADTS frames are split and pushed with cumulative PTS.
/// If `metadata` is provided, AI detection tags and recording times are embedded as MP4 tags.
pub fn mux_nals_to_mp4(
    nals: &[Vec<u8>],
    timestamps_us: &[u32],
    aac_data: &[u8],
    output_path: &Path,
    metadata: &Mp4Metadata,
    is_h265: bool,
) -> Result<()> {
    gstreamer::init().context("GStreamer init")?;

    let pipeline = Pipeline::new();

    // --- Video path ---
    let video_src = gstreamer::ElementFactory::make("appsrc")
        .build()
        .context("create video appsrc")?
        .downcast::<AppSrc>()
        .map_err(|_| anyhow::anyhow!("appsrc downcast"))?;
    let video_parse_name = if is_h265 { "h265parse" } else { "h264parse" };
    let h264parse = gstreamer::ElementFactory::make(video_parse_name)
        .build()
        .context("create video parse element")?;
    let mp4mux = gstreamer::ElementFactory::make("mp4mux")
        .build()
        .context("create mp4mux")?;
    mp4mux.set_property("faststart", true);
    let filesink = gstreamer::ElementFactory::make("filesink")
        .build()
        .context("create filesink")?;
    filesink.set_property("location", output_path);

    let video_src_el = video_src.upcast_ref();
    pipeline.add_many([video_src_el, &h264parse, &mp4mux, &filesink])?;
    video_src_el.link(&h264parse)?;
    h264parse.link(&mp4mux)?;
    mp4mux.link(&filesink)?;

    let video_media_type = if is_h265 { "video/x-h265" } else { "video/x-h264" };
    let caps = Caps::builder(video_media_type)
        .field("stream-format", "byte-stream")
        .build();
    video_src.set_caps(Some(&caps));
    video_src.set_stream_type(gstreamer_app::AppStreamType::Stream);
    video_src.set_format(gstreamer::Format::Time);

    // --- Audio path (optional) ---
    let audio_src = if !aac_data.is_empty() {
        let src = gstreamer::ElementFactory::make("appsrc")
            .build()
            .context("create audio appsrc")?
            .downcast::<AppSrc>()
            .map_err(|_| anyhow::anyhow!("audio appsrc downcast"))?;
        let aacparse = gstreamer::ElementFactory::make("aacparse")
            .build()
            .context("create aacparse")?;

        let src_el = src.upcast_ref();
        pipeline.add_many([src_el, &aacparse])?;
        src_el.link(&aacparse)?;
        aacparse.link(&mp4mux)?;

        let caps = Caps::builder("audio/mpeg")
            .field("mpegversion", 4i32)
            .field("stream-format", "adts")
            .build();
        src.set_caps(Some(&caps));
        src.set_stream_type(gstreamer_app::AppStreamType::Stream);
        src.set_format(gstreamer::Format::Time);
        Some(src)
    } else {
        None
    };

    // Embed metadata as MP4 tags via mp4mux's TagSetter interface
    if metadata.record_type.is_some()
        || metadata.start_time.is_some()
        || metadata.end_time.is_some()
        || metadata.camera_name.is_some()
    {
        let tag_setter = mp4mux
            .dynamic_cast_ref::<gstreamer::TagSetter>()
            .expect("mp4mux should implement TagSetter");
        // AI detection labels as keywords
        if let Some(ref rt) = metadata.record_type {
            let ai_tags: Vec<&str> = rt
                .split(',')
                .map(|s| s.trim())
                .filter(|s| {
                    matches!(
                        *s,
                        "people" | "vehicle" | "face" | "dog_cat" | "package"
                            | "visitor" | "cry" | "crossline" | "intrusion"
                            | "loitering" | "nonmotorveh" | "md" | "pir"
                            | "io" | "other" | "legacy" | "loss"
                    )
                })
                .collect();
            if !ai_tags.is_empty() {
                let kw = ai_tags.join(", ");
                tag_setter.add_tag::<gstreamer::tags::Keywords>(
                    &kw.as_str(),
                    gstreamer::TagMergeMode::Replace,
                );
            }
            let comment = format!("recordType: {}", rt);
            tag_setter.add_tag::<gstreamer::tags::Comment>(
                &comment.as_str(),
                gstreamer::TagMergeMode::Replace,
            );
        }
        // Build description from timing + camera info
        let mut desc_parts = Vec::new();
        if let Some(ref name) = metadata.camera_name {
            desc_parts.push(format!("Camera: {}", name));
            tag_setter.add_tag::<gstreamer::tags::Title>(
                &name.as_str(),
                gstreamer::TagMergeMode::Replace,
            );
        }
        if let Some(ref st) = metadata.start_time {
            desc_parts.push(format!("Start: {}", st));
        }
        if let Some(ref et) = metadata.end_time {
            desc_parts.push(format!("End: {}", et));
        }
        if !desc_parts.is_empty() {
            let desc = desc_parts.join("; ");
            tag_setter.add_tag::<gstreamer::tags::Description>(
                &desc.as_str(),
                gstreamer::TagMergeMode::Replace,
            );
        }
        tag_setter.add_tag::<gstreamer::tags::Encoder>(
            &"neolink replay",
            gstreamer::TagMergeMode::Replace,
        );
        log::info!("Replay GStreamer: set metadata tags on mp4mux TagSetter");
    }

    pipeline.set_state(State::Playing)?;

    // Push video NALs with per-frame PTS
    let base_ts = timestamps_us.first().copied().unwrap_or(0);
    // Compute average frame duration for use on the last frame
    let avg_frame_dur_us: u32 = if timestamps_us.len() > 1 {
        (timestamps_us.last().unwrap().wrapping_sub(*timestamps_us.first().unwrap()))
            / (timestamps_us.len() as u32 - 1)
    } else {
        33333 // ~30fps fallback
    };
    for (i, nal) in nals.iter().enumerate() {
        let ts_us = timestamps_us.get(i).copied().unwrap_or(0);
        let pts_us = ts_us.wrapping_sub(base_ts) as u64;
        let pts_ns = pts_us * 1000;
        let next_ts_us = timestamps_us.get(i + 1).copied().unwrap_or_else(|| ts_us.wrapping_add(avg_frame_dur_us));
        let dur_ns = (next_ts_us.wrapping_sub(ts_us) as u64) * 1000;

        // Ensure Annex B start code
        let has_start = (nal.len() >= 4 && nal[0..4] == [0, 0, 0, 1])
            || (nal.len() >= 3 && nal[0..3] == [0, 0, 1]);
        let data_len = if has_start { nal.len() } else { START_CODE.len() + nal.len() };

        let mut buf = gstreamer::Buffer::with_size(data_len).context("video buffer alloc")?;
        {
            let buf_ref = buf.get_mut().unwrap();
            buf_ref.set_pts(ClockTime::from_nseconds(pts_ns));
            buf_ref.set_duration(ClockTime::from_nseconds(dur_ns));
            let mut map = buf_ref.map_writable().context("video buffer map")?;
            if has_start {
                map.copy_from_slice(nal);
            } else {
                map[..START_CODE.len()].copy_from_slice(START_CODE);
                map[START_CODE.len()..].copy_from_slice(nal);
            }
        }
        video_src
            .push_buffer(buf)
            .map_err(|e| anyhow::anyhow!("video push frame {}: {:?}", i, e))?;
    }
    video_src
        .end_of_stream()
        .map_err(|e| anyhow::anyhow!("video eos: {:?}", e))?;

    // Push AAC ADTS frames with cumulative PTS
    if let Some(ref audio) = audio_src {
        let mut pos = 0;
        let mut audio_pts_ns: u64 = 0;
        while pos + 7 <= aac_data.len() {
            // ADTS syncword 0xFFF
            if aac_data[pos] != 0xFF || (aac_data[pos + 1] & 0xF0) != 0xF0 {
                pos += 1;
                continue;
            }
            let frame_len = (((aac_data[pos + 3] & 0x03) as usize) << 11)
                | ((aac_data[pos + 4] as usize) << 3)
                | ((aac_data[pos + 5] as usize) >> 5);
            if frame_len < 7 || pos + frame_len > aac_data.len() {
                break;
            }

            // Duration from ADTS: sample_freq, num_frames (each 1024 samples)
            let freq_idx = (aac_data[pos + 2] & 0x3C) >> 2;
            let sample_freq: u64 = match freq_idx {
                0 => 96000, 1 => 88200, 2 => 64000, 3 => 48000, 4 => 44100,
                5 => 32000, 6 => 24000, 7 => 22050, 8 => 16000, 9 => 12000,
                10 => 11025, 11 => 8000, 12 => 7350, _ => 16000,
            };
            let num_frames = ((aac_data[pos + 6] & 0x03) + 1) as u64;
            let samples = num_frames * 1024;
            let duration_ns = samples * 1_000_000_000 / sample_freq;

            let mut buf =
                gstreamer::Buffer::with_size(frame_len).context("audio buffer alloc")?;
            {
                let buf_ref = buf.get_mut().unwrap();
                buf_ref.set_pts(ClockTime::from_nseconds(audio_pts_ns));
                buf_ref.set_duration(ClockTime::from_nseconds(duration_ns));
                let mut map = buf_ref.map_writable().context("audio buffer map")?;
                map.copy_from_slice(&aac_data[pos..pos + frame_len]);
            }
            audio
                .push_buffer(buf)
                .map_err(|e| anyhow::anyhow!("audio push: {:?}", e))?;

            audio_pts_ns += duration_ns;
            pos += frame_len;
        }
        audio
            .end_of_stream()
            .map_err(|e| anyhow::anyhow!("audio eos: {:?}", e))?;
        log::info!(
            "Replay GStreamer: pushed {} ms of AAC audio",
            audio_pts_ns / 1_000_000
        );
    }

    // Wait for EOS
    let bus = pipeline.bus().context("pipeline bus")?;
    for msg in bus.iter_timed(ClockTime::NONE) {
        match msg.view() {
            MessageView::Eos(..) => break,
            MessageView::Error(err) => {
                let _ = pipeline.set_state(State::Null);
                anyhow::bail!("GStreamer error: {:?}", err);
            }
            _ => {}
        }
    }

    pipeline.set_state(State::Null)?;
    log::info!(
        "Replay GStreamer: muxed {} {} frames + audio to {}",
        nals.len(),
        if is_h265 { "H.265" } else { "H.264" },
        output_path.display()
    );
    Ok(())
}
