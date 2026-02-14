use anyhow::{Context, Result};
use neolink_core::bc::xml::StreamCompression;

mod cmdline;

use crate::common::NeoReactor;
pub(crate) use cmdline::Opt;
use cmdline::{EncodingAction, StreamName};

fn print_stream(name: &str, stream: &Option<StreamCompression>) {
    match stream {
        Some(s) => {
            println!(
                "  {:<8} {:>5}x{:<5} {:>6} kbps  {:>3} fps  {:<4} {}",
                name,
                s.width.unwrap_or(0),
                s.height.unwrap_or(0),
                s.bit_rate.unwrap_or(0),
                s.frame.unwrap_or(0),
                s.encoder_type.as_deref().unwrap_or("-"),
                s.encoder_profile.as_deref().unwrap_or("-"),
            );
        }
        None => {
            println!("  {:<8} (not available)", name);
        }
    }
}

pub(crate) async fn main(opt: Opt, reactor: NeoReactor) -> Result<()> {
    let camera = reactor.get(&opt.camera).await?;

    match opt.cmd {
        EncodingAction::Get => {
            let compression = camera
                .run_task(|cam| {
                    Box::pin(async move {
                        cam.get_compression()
                            .await
                            .context("Unable to get encoding settings")
                    })
                })
                .await?;

            println!("Encoding settings (channel {}):", compression.channel_id);
            println!(
                "  {:<8} {:>10}       {:>10}  {:>7}  {:<4} {}",
                "Stream", "Resolution", "Bitrate", "FPS", "Type", "Profile"
            );
            println!("  {}", "-".repeat(64));
            print_stream("main", &compression.main_stream);
            print_stream("sub", &compression.sub_stream);
            print_stream("third", &compression.third_stream);
        }
        EncodingAction::Set {
            stream,
            bitrate,
            fps,
            rate_control,
            profile,
        } => {
            if bitrate.is_none()
                && fps.is_none()
                && rate_control.is_none()
                && profile.is_none()
            {
                anyhow::bail!(
                    "At least one of --bitrate, --fps, --rate-control, or --profile must be specified"
                );
            }

            let stream_name = stream.clone();
            camera
                .run_task(move |cam| {
                    let rate_control = rate_control.clone();
                    let profile = profile.clone();
                    let stream = stream_name.clone();
                    Box::pin(async move {
                        let mut compression = cam
                            .get_compression()
                            .await
                            .context("Unable to get current encoding settings")?;

                        let target = match stream {
                            StreamName::Main => &mut compression.main_stream,
                            StreamName::Sub => &mut compression.sub_stream,
                            StreamName::Third => &mut compression.third_stream,
                        };

                        let s = target.get_or_insert_with(Default::default);

                        if let Some(br) = bitrate {
                            s.bit_rate = Some(br);
                        }
                        if let Some(f) = fps {
                            s.frame = Some(f);
                        }
                        if let Some(ref rc) = rate_control {
                            s.encoder_type = Some(rc.to_string());
                        }
                        if let Some(ref p) = profile {
                            s.encoder_profile = Some(p.to_string());
                        }

                        cam.set_compression(compression)
                            .await
                            .context("Unable to set encoding settings")?;

                        println!("Encoding settings updated successfully.");
                        Ok(())
                    })
                })
                .await?;
        }
    }

    Ok(())
}
