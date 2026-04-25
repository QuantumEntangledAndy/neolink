//! HDD/SD disk list and format (MSG 102, MSG 103).

use super::{BcCamera, Error, Result};
use crate::bc::{
    model::*,
    xml::{xml_ver, BcPayloads, BcXml, FormatExpandCfg, HddInit, HddInitList, HddInfoList},
};

impl BcCamera {
    /// Get the HDD/SD disk list (MSG 102, HddInfoList).
    /// Returns the list of disks/slots with capacity, mount and remain size.
    pub async fn get_hdd_list(&self) -> Result<HddInfoList> {
        let connection = self.get_connection();
        let msg_num = self.new_message_num();
        let mut sub = connection
            .subscribe(MSG_ID_HDD_INFO_LIST, msg_num)
            .await?;

        let msg = Bc {
            meta: BcMeta {
                msg_id: MSG_ID_HDD_INFO_LIST,
                channel_id: self.channel_id,
                msg_num,
                stream_type: 0,
                response_code: 0,
                class: 0x6414,
            },
            body: BcBody::ModernMsg(ModernMsg {
                extension: None,
                payload: None,
            }),
        };

        sub.send(msg).await?;
        let msg = sub.recv().await?;

        if msg.meta.response_code != 200 {
            return Err(Error::CameraServiceUnavailable {
                id: msg.meta.msg_id,
                code: msg.meta.response_code,
            });
        }

        if let BcBody::ModernMsg(ModernMsg {
            payload:
                Some(BcPayloads::BcXml(BcXml {
                    hdd_info_list: Some(data),
                    ..
                })),
            ..
        }) = msg.body
        {
            Ok(data)
        } else {
            Err(Error::UnintelligibleReply {
                _reply: std::sync::Arc::new(Box::new(msg)),
                why: "Expected HddInfoList xml but it was not received",
            })
        }
    }

    /// Format one or more disks (MSG 103, HddInitList).
    /// `init_ids`: disk/slot numbers (from [get_hdd_list](Self::get_hdd_list), e.g. `HddInfo.number`).
    /// `full_format`: `false` = quick format, `true` = full format.
    pub async fn format_disk(&self, init_ids: &[u8], full_format: bool) -> Result<()> {
        if init_ids.is_empty() {
            return Err(Error::InvalidArgument {
                argument: "init_ids".to_string(),
                value: "must specify at least one disk to format".to_string(),
            });
        }

        let connection = self.get_connection();
        let msg_num = self.new_message_num();
        let mut sub = connection
            .subscribe(MSG_ID_HDD_INIT_LIST, msg_num)
            .await?;

        let hdd_init_list = HddInitList {
            version: xml_ver(),
            hdd_init: init_ids
                .iter()
                .map(|&init_id| HddInit {
                    init_id,
                    type_: if full_format { 1 } else { 0 },
                })
                .collect(),
        };

        let xml = BcXml {
            format_expand_cfg: Some(FormatExpandCfg { hdd_init_list }),
            ..Default::default()
        };

        let msg = Bc {
            meta: BcMeta {
                msg_id: MSG_ID_HDD_INIT_LIST,
                channel_id: self.channel_id,
                msg_num,
                stream_type: 0,
                response_code: 0,
                class: 0x6414,
            },
            body: BcBody::ModernMsg(ModernMsg {
                extension: None,
                payload: Some(BcPayloads::BcXml(xml)),
            }),
        };

        sub.send(msg).await?;
        let msg = sub.recv().await?;

        if msg.meta.response_code == 200 {
            Ok(())
        } else {
            Err(Error::UnintelligibleReply {
                _reply: std::sync::Arc::new(Box::new(msg)),
                why: "Camera did not accept the format command",
            })
        }
    }
}
