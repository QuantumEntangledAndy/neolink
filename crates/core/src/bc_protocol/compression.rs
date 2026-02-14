use super::{BcCamera, Error, Result};
use crate::bc::{model::*, xml::*};

impl BcCamera {
    /// Get the current compression/encoding settings
    pub async fn get_compression(&self) -> Result<Compression> {
        self.has_ability_ro("compress").await?;
        let connection = self.get_connection();
        let msg_num = self.new_message_num();
        let mut sub_get = connection.subscribe(MSG_ID_GET_COMPRESSION, msg_num).await?;
        let get = Bc {
            meta: BcMeta {
                msg_id: MSG_ID_GET_COMPRESSION,
                channel_id: self.channel_id,
                msg_num,
                response_code: 0,
                stream_type: 0,
                class: 0x6414,
            },
            body: BcBody::ModernMsg(ModernMsg {
                extension: Some(Extension {
                    channel_id: Some(self.channel_id),
                    ..Default::default()
                }),
                payload: None,
            }),
        };

        sub_get.send(get).await?;
        let msg = sub_get.recv().await?;
        if msg.meta.response_code != 200 {
            return Err(Error::CameraServiceUnavailable {
                id: msg.meta.msg_id,
                code: msg.meta.response_code,
            });
        }

        if let BcBody::ModernMsg(ModernMsg {
            payload:
                Some(BcPayloads::BcXml(BcXml {
                    compression: Some(compression),
                    ..
                })),
            ..
        }) = msg.body
        {
            Ok(compression)
        } else {
            Err(Error::UnintelligibleReply {
                reply: std::sync::Arc::new(Box::new(msg)),
                why: "Expected Compression xml but it was not received",
            })
        }
    }

    /// Set compression/encoding settings
    pub async fn set_compression(&self, compression: Compression) -> Result<()> {
        self.has_ability_rw("compress").await?;
        let connection = self.get_connection();

        let msg_num = self.new_message_num();
        let mut sub_set = connection.subscribe(MSG_ID_SET_COMPRESSION, msg_num).await?;

        let set = Bc {
            meta: BcMeta {
                msg_id: MSG_ID_SET_COMPRESSION,
                channel_id: self.channel_id,
                msg_num,
                response_code: 0,
                stream_type: 0,
                class: 0x6414,
            },
            body: BcBody::ModernMsg(ModernMsg {
                extension: Some(Extension {
                    channel_id: Some(self.channel_id),
                    ..Default::default()
                }),
                payload: Some(BcPayloads::BcXml(BcXml {
                    compression: Some(compression),
                    ..Default::default()
                })),
            }),
        };

        sub_set.send(set).await?;
        if let Ok(reply) =
            tokio::time::timeout(tokio::time::Duration::from_millis(500), sub_set.recv()).await
        {
            let msg = reply?;

            if let BcMeta {
                response_code: 200, ..
            } = msg.meta
            {
                Ok(())
            } else {
                Err(Error::UnintelligibleReply {
                    reply: std::sync::Arc::new(Box::new(msg)),
                    why: "The camera did not accept the Compression xml",
                })
            }
        } else {
            // Some cameras seem to just not send a reply on success
            Ok(())
        }
    }
}
