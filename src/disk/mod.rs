//!
//! # Neolink Disk
//!
//! List HDD/SD disks and format disk(s) (MSG 102, MSG 103).
//!
//! # Usage
//!
//! ```bash
//! neolink disk list --config=config.toml CameraName
//! neolink disk format --config=config.toml CameraName --disk 0
//! neolink disk format --config=config.toml CameraName --disk 0 --full
//! ```
//!

use anyhow::{Context, Result};

mod cmdline;

use crate::common::NeoReactor;
use neolink_core::bc::xml::HddInfoList;

pub(crate) use cmdline::Opt;

/// Entry point for the disk subcommand
pub(crate) async fn main(opt: Opt, reactor: NeoReactor) -> Result<()> {
    let camera = reactor.get(&opt.camera).await?;
    let cmd = opt.cmd.unwrap_or(cmdline::DiskCommand::List);

    match cmd {
        cmdline::DiskCommand::List => {
            let list = camera
                .run_task(|cam| {
                    Box::pin(
                        async move { cam.get_hdd_list().await.context("Could not get disk list from camera") },
                    )
                })
                .await?;

            print_hdd_list(&list);
        }
        cmdline::DiskCommand::Format { disk, full } => {
            if disk.is_empty() {
                anyhow::bail!("At least one disk must be specified (e.g. --disk 0)");
            }
            camera
                .run_task(|cam| {
                    let disks = disk.clone();
                    Box::pin(
                        async move {
                            cam.format_disk(&disks, full)
                                .await
                                .context("Could not send format command to camera")
                        },
                    )
                })
                .await?;
            println!("Format command sent successfully.");
        }
    }

    Ok(())
}

fn print_hdd_list(list: &HddInfoList) {
    if list.hdd_info.is_empty() {
        println!("No disks found.");
        return;
    }
    println!("Disks:");
    for h in &list.hdd_info {
        let cap = h
            .capacity
            .map(|c| format!("{} GB", c))
            .unwrap_or_else(|| "—".to_string());
        let remain = h
            .remain_size
            .map(|r| format!("{} GB free", r))
            .or(h.remain_size_m.map(|r| format!("{} MB free", r)))
            .unwrap_or_else(|| "—".to_string());
        let mount = h
            .mount
            .map(|m| if m != 0 { "mounted" } else { "not mounted" })
            .unwrap_or("—");
        let fmt = h
            .format
            .map(|f| if f != 0 { "formatted" } else { "not formatted" })
            .unwrap_or("—");
        println!(
            "  Slot {}: capacity {}, {}, {}, {}",
            h.number, cap, remain, mount, fmt
        );
    }
}
