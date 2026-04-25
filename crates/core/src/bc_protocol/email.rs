//! Email controlling methods
//!
use super::{BcCamera, Error, Result};
use crate::bc::{model::*, xml::*};

impl BcCamera {
    /// Get the current Email XML
    pub async fn get_email(&self) -> Result<Email> {
        let connection = self.get_connection();
        let msg_num = self.new_message_num();
        let mut sub_get = connection.subscribe(MSG_ID_GET_EMAIL, msg_num).await?;
        let get = Bc {
            meta: BcMeta {
                msg_id: MSG_ID_GET_EMAIL,
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
        if msg.meta.response_code != 200 {
            return Err(Error::CameraServiceUnavailable {
                id: msg.meta.msg_id,
                code: msg.meta.response_code,
            });
        }

        if let BcBody::ModernMsg(ModernMsg {
            payload:
                Some(BcPayloads::BcXml(BcXml {
                    email: Some(email), ..
                })),
            ..
        }) = msg.body
        {
            Ok(email)
        } else {
            Err(Error::UnintelligibleReply {
                _reply: std::sync::Arc::new(Box::new(msg)),
                why: "Expected Email xml but it was not recieved",
            })
        }
    }

    /// Set the Email XML
    pub async fn set_email(&self, email: Email) -> Result<()> {
        let connection = self.get_connection();
        let msg_num = self.new_message_num();
        let mut sub_get = connection.subscribe(MSG_ID_SET_EMAIL, msg_num).await?;
        let get = Bc {
            meta: BcMeta {
                msg_id: MSG_ID_SET_EMAIL,
                channel_id: self.channel_id,
                msg_num,
                response_code: 0,
                stream_type: 0,
                class: 0x6414,
            },
            body: BcBody::ModernMsg(ModernMsg {
                extension: None,
                payload: Some(BcPayloads::BcXml(BcXml {
                    email: Some(email),
                    ..Default::default()
                })),
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
        Ok(())
    }

    /// Test the Email with this XML
    pub async fn test_email(&self, email: Email) -> Result<()> {
        let connection = self.get_connection();
        let msg_num = self.new_message_num();
        let mut sub_get = connection.subscribe(MSG_ID_TEST_EMAIL, msg_num).await?;
        let get = Bc {
            meta: BcMeta {
                msg_id: MSG_ID_TEST_EMAIL,
                channel_id: self.channel_id,
                msg_num,
                response_code: 0,
                stream_type: 0,
                class: 0x6414,
            },
            body: BcBody::ModernMsg(ModernMsg {
                extension: None,
                payload: Some(BcPayloads::BcXml(BcXml {
                    email: Some(email),
                    ..Default::default()
                })),
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
        Ok(())
    }

    /// Get the current EmailTask XML
    pub async fn get_email_task(&self) -> Result<EmailTask> {
        let connection = self.get_connection();
        let msg_num = self.new_message_num();
        let mut sub_get = connection.subscribe(MSG_ID_GET_EMAIL_TASK, msg_num).await?;
        let get = Bc {
            meta: BcMeta {
                msg_id: MSG_ID_GET_EMAIL_TASK,
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
                    email_task: Some(email_task),
                    ..
                })),
            ..
        }) = msg.body
        {
            Ok(email_task)
        } else {
            Err(Error::UnintelligibleReply {
                _reply: std::sync::Arc::new(Box::new(msg)),
                why: "Expected EmailTask xml but it was not recieved",
            })
        }
    }

    /// Setup the Email Task
    pub async fn set_email_task(&self, email_task: EmailTask) -> Result<()> {
        let connection = self.get_connection();
        let msg_num = self.new_message_num();
        let mut sub_get = connection.subscribe(MSG_ID_SET_EMAIL_TASK, msg_num).await?;
        let get = Bc {
            meta: BcMeta {
                msg_id: MSG_ID_SET_EMAIL_TASK,
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
                    email_task: Some(email_task),
                    ..Default::default()
                })),
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
        Ok(())
    }

    /// Turn on Email notifications
    pub async fn email_on(&self) -> Result<()> {
        self.set_email_task(EmailTask {
            version: xml_ver(),
            channel_id: self.channel_id,
            enable: 1,
            schedule_list: None,
        })
        .await
    }

    /// Turn off Email notifications
    pub async fn email_off(&self) -> Result<()> {
        self.set_email_task(EmailTask {
            version: xml_ver(),
            channel_id: self.channel_id,
            enable: 0,
            schedule_list: None,
        })
        .await
    }

    /// Turn on Email notifications all the time
    pub async fn email_on_always(&self) -> Result<()> {
        const DOW: [&str; 7] = [
            "Sunday",
            "Monday",
            "Tuesday",
            "Wednesday",
            "Thursday",
            "Friday",
            "Saturday",
        ];
        self.set_email_task(EmailTask {
            version: xml_ver(),
            channel_id: self.channel_id,
            enable: 1,
            schedule_list: Some(ScheduleList {
                schedule: Schedule {
                    alarm_type: "MD".to_owned(),
                    time_block_list: TimeBlockList {
                        time_block: DOW
                            .iter()
                            .map(|d| TimeBlock {
                                enable: 1,
                                week_day: d.to_string(),
                                begin_hour: 0,
                                end_hour: 23,
                            })
                            .collect(),
                    },
                },
            }),
        })
        .await
    }
}
