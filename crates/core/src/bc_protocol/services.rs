use super::{BcCamera, Error, Result};
use crate::bc::{model::*, xml::*};
use tokio::time::{interval, Duration};

impl BcCamera {
    /// Helper to set the service state since they all share the same code
    /// No checks are made to ensure the xml is valid service xml
    ///   hence private method
    async fn set_services(&self, bcxml: BcXml) -> Result<()> {
        let connection = self.get_connection();
        let msg_num = self.new_message_num();
        let mut sub_set = connection
            .subscribe(MSG_ID_SET_SERVICE_PORTS, msg_num)
            .await?;

        let get = Bc {
            meta: BcMeta {
                msg_id: MSG_ID_SET_SERVICE_PORTS,
                channel_id: self.channel_id,
                msg_num,
                response_code: 0,
                stream_type: 0,
                class: 0x6414,
            },
            body: BcBody::ModernMsg(ModernMsg {
                extension: None,
                payload: Some(BcPayloads::BcXml(bcxml)),
            }),
        };

        sub_set.send(get).await?;
        if let Ok(reply) =
            tokio::time::timeout(tokio::time::Duration::from_millis(500), sub_set.recv()).await
        {
            let msg = reply?;
            if msg.meta.response_code != 200 {
                return Err(Error::CameraServiceUnavailable {
                    id: msg.meta.msg_id,
                    code: msg.meta.response_code,
                });
            }

            if let BcMeta {
                response_code: 200, ..
            } = msg.meta
            {
                Ok(())
            } else {
                Err(Error::UnintelligibleReply {
                    _reply: std::sync::Arc::new(Box::new(msg)),
                    why: "The camera did not except the BcXmp with service data",
                })
            }
        } else {
            // Some cameras seem to just not send a reply on success, so after 500ms we return Ok
            Ok(())
        }
    }

    /// Helper since they all send the same message
    /// No checks are made to ensure the xml is valid service xml
    ///   hence private method
    async fn get_services(&self) -> Result<BcXml> {
        let connection = self.get_connection();
        let mut reties: usize = 0;
        let mut retry_interval = interval(Duration::from_millis(500));
        loop {
            retry_interval.tick().await;
            let msg_num = self.new_message_num();
            let mut sub_get = connection
                .subscribe(MSG_ID_GET_SERVICE_PORTS, msg_num)
                .await?;
            let get = Bc {
                meta: BcMeta {
                    msg_id: MSG_ID_GET_SERVICE_PORTS,
                    channel_id: self.channel_id,
                    msg_num,
                    response_code: 0,
                    stream_type: 0,
                    class: 0x6414,
                },
                body: BcBody::ModernMsg(ModernMsg {
                    extension: None,
                    payload: None,
                }),
            };

            sub_get.send(get).await?;
            let msg = sub_get.recv().await?;
            if msg.meta.response_code == 400 {
                // Retryable
                if reties < 5 {
                    reties += 1;
                    continue;
                } else {
                    return Err(Error::CameraServiceUnavailable {
                        id: msg.meta.msg_id,
                        code: msg.meta.response_code,
                    });
                }
            } else if msg.meta.response_code != 200 {
                return Err(Error::CameraServiceUnavailable {
                    id: msg.meta.msg_id,
                    code: msg.meta.response_code,
                });
            } else {
                // Valid message with response_code == 200
                if let BcBody::ModernMsg(ModernMsg {
                    payload: Some(BcPayloads::BcXml(xml)),
                    ..
                }) = msg.body
                {
                    return Ok(xml);
                } else {
                    return Err(Error::UnintelligibleReply {
                        _reply: std::sync::Arc::new(Box::new(msg)),
                        why: "Expected ModernMsg payload but it was not recieved",
                    });
                }
            }
        }
    }

    /// Get the [`ServerPort`] XML
    pub async fn get_serverport(&self) -> Result<ServerPort> {
        let bcxml = self.get_services().await?;
        if let BcXml {
            server_port: Some(xml),
            ..
        } = bcxml
        {
            Ok(xml)
        } else {
            Err(Error::UnintelligibleXml {
                _reply: std::sync::Arc::new(Box::new(bcxml)),
                why: "Expected ServerPort xml but it was not recieved",
            })
        }
    }

    /// Set the server port
    pub async fn set_serverport(&self, set_on: Option<bool>, set_port: Option<u32>) -> Result<()> {
        let bcxml = self.get_services().await?;
        if let BcXml {
            server_port: Some(mut xml),
            ..
        } = bcxml
        {
            if let Some(enabled) = set_on {
                xml.enable = Some({
                    if enabled {
                        1
                    } else {
                        0
                    }
                });
            }
            if let Some(port) = set_port {
                xml.port = port;
            }
            self.set_services(BcXml {
                server_port: Some(xml),
                ..Default::default()
            })
            .await
        } else {
            Err(Error::UnintelligibleXml {
                _reply: std::sync::Arc::new(Box::new(bcxml)),
                why: "Expected ServerPort xml but it was not recieved",
            })
        }
    }

    /// Get the [`HttpPort`] XML
    pub async fn get_http(&self) -> Result<HttpPort> {
        let bcxml = self.get_services().await?;
        if let BcXml {
            http_port: Some(xml),
            ..
        } = bcxml
        {
            Ok(xml)
        } else {
            Err(Error::UnintelligibleXml {
                _reply: std::sync::Arc::new(Box::new(bcxml)),
                why: "Expected HttpPort xml but it was not recieved",
            })
        }
    }

    /// Set the http port
    pub async fn set_http(&self, set_on: Option<bool>, set_port: Option<u32>) -> Result<()> {
        let bcxml = self.get_services().await?;
        if let BcXml {
            http_port: Some(mut xml),
            ..
        } = bcxml
        {
            if let Some(enabled) = set_on {
                xml.enable = Some({
                    if enabled {
                        1
                    } else {
                        0
                    }
                });
            }
            if let Some(port) = set_port {
                xml.port = port;
            }
            self.set_services(BcXml {
                http_port: Some(xml),
                ..Default::default()
            })
            .await
        } else {
            Err(Error::UnintelligibleXml {
                _reply: std::sync::Arc::new(Box::new(bcxml)),
                why: "Expected HttpPort xml but it was not recieved",
            })
        }
    }

    /// Get the [`HttpPort`] XML
    pub async fn get_https(&self) -> Result<HttpsPort> {
        let bcxml = self.get_services().await?;
        if let BcXml {
            https_port: Some(xml),
            ..
        } = bcxml
        {
            Ok(xml)
        } else {
            Err(Error::UnintelligibleXml {
                _reply: std::sync::Arc::new(Box::new(bcxml)),
                why: "Expected HttpsPort xml but it was not recieved",
            })
        }
    }

    /// Set the https port
    pub async fn set_https(&self, set_on: Option<bool>, set_port: Option<u32>) -> Result<()> {
        let bcxml = self.get_services().await?;
        if let BcXml {
            https_port: Some(mut xml),
            ..
        } = bcxml
        {
            if let Some(enabled) = set_on {
                xml.enable = Some({
                    if enabled {
                        1
                    } else {
                        0
                    }
                });
            }
            if let Some(port) = set_port {
                xml.port = port;
            }
            self.set_services(BcXml {
                https_port: Some(xml),
                ..Default::default()
            })
            .await
        } else {
            Err(Error::UnintelligibleXml {
                _reply: std::sync::Arc::new(Box::new(bcxml)),
                why: "Expected HttpsPort xml but it was not recieved",
            })
        }
    }

    /// Get the [`RtspPort`] XML
    pub async fn get_rtsp(&self) -> Result<RtspPort> {
        let bcxml = self.get_services().await?;
        if let BcXml {
            rtsp_port: Some(xml),
            ..
        } = bcxml
        {
            Ok(xml)
        } else {
            Err(Error::UnintelligibleXml {
                _reply: std::sync::Arc::new(Box::new(bcxml)),
                why: "Expected RtspPort xml but it was not recieved",
            })
        }
    }

    /// Set the http port
    pub async fn set_rtsp(&self, set_on: Option<bool>, set_port: Option<u32>) -> Result<()> {
        let bcxml = self.get_services().await?;
        if let BcXml {
            rtsp_port: Some(mut xml),
            ..
        } = bcxml
        {
            if let Some(enabled) = set_on {
                xml.enable = Some({
                    if enabled {
                        1
                    } else {
                        0
                    }
                });
            }
            if let Some(port) = set_port {
                xml.port = port;
            }
            self.set_services(BcXml {
                rtsp_port: Some(xml),
                ..Default::default()
            })
            .await
        } else {
            Err(Error::UnintelligibleXml {
                _reply: std::sync::Arc::new(Box::new(bcxml)),
                why: "Expected RtspPort xml but it was not recieved",
            })
        }
    }

    /// Get the [`RtmpPort`] XML
    pub async fn get_rtmp(&self) -> Result<RtmpPort> {
        let bcxml = self.get_services().await?;
        if let BcXml {
            rtmp_port: Some(xml),
            ..
        } = bcxml
        {
            Ok(xml)
        } else {
            Err(Error::UnintelligibleXml {
                _reply: std::sync::Arc::new(Box::new(bcxml)),
                why: "Expected RtmpPort xml but it was not recieved",
            })
        }
    }

    /// Set the rtmp port
    pub async fn set_rtmp(&self, set_on: Option<bool>, set_port: Option<u32>) -> Result<()> {
        let bcxml = self.get_services().await?;
        if let BcXml {
            rtmp_port: Some(mut xml),
            ..
        } = bcxml
        {
            if let Some(enabled) = set_on {
                xml.enable = Some({
                    if enabled {
                        1
                    } else {
                        0
                    }
                });
            }
            if let Some(port) = set_port {
                xml.port = port;
            }
            self.set_services(BcXml {
                rtmp_port: Some(xml),
                ..Default::default()
            })
            .await
        } else {
            Err(Error::UnintelligibleXml {
                _reply: std::sync::Arc::new(Box::new(bcxml)),
                why: "Expected RtmpPort xml but it was not recieved",
            })
        }
    }

    /// Get the [`OnvifPort`] XML
    pub async fn get_onvif(&self) -> Result<OnvifPort> {
        let bcxml = self.get_services().await?;
        if let BcXml {
            onvif_port: Some(xml),
            ..
        } = bcxml
        {
            Ok(xml)
        } else {
            Err(Error::UnintelligibleXml {
                _reply: std::sync::Arc::new(Box::new(bcxml)),
                why: "Expected OnvifPort xml but it was not recieved",
            })
        }
    }

    /// Set the onvif port
    pub async fn set_onvif(&self, set_on: Option<bool>, set_port: Option<u32>) -> Result<()> {
        let bcxml = self.get_services().await?;
        if let BcXml {
            onvif_port: Some(mut xml),
            ..
        } = bcxml
        {
            if let Some(enabled) = set_on {
                xml.enable = Some({
                    if enabled {
                        1
                    } else {
                        0
                    }
                });
            }
            if let Some(port) = set_port {
                xml.port = port;
            }
            self.set_services(BcXml {
                onvif_port: Some(xml),
                ..Default::default()
            })
            .await
        } else {
            Err(Error::UnintelligibleXml {
                _reply: std::sync::Arc::new(Box::new(bcxml)),
                why: "Expected OnvifPort xml but it was not recieved",
            })
        }
    }
}
