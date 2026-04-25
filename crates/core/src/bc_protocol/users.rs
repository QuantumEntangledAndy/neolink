use super::{BcCamera, Error, Result};
use crate::bc::{model::*, xml::*};

impl BcCamera {
    /// Returns all users configured in the camera
    pub async fn get_users(&self) -> Result<UserList> {
        let connection = self.get_connection();

        let msg_num = self.new_message_num();
        let mut sub_get = connection
            .subscribe(MSG_ID_GET_ABILITY_SUPPORT, msg_num)
            .await?;
        let get = Bc {
            meta: BcMeta {
                msg_id: MSG_ID_GET_ABILITY_SUPPORT,
                channel_id: self.channel_id,
                msg_num,
                response_code: 0,
                stream_type: 0,
                class: 0x6414,
            },
            body: BcBody::ModernMsg(ModernMsg {
                extension: Some(Extension {
                    user_name: Some("admin".to_owned()),
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

        // Valid message with response_code == 200
        if let BcBody::ModernMsg(ModernMsg {
            payload:
                Some(BcPayloads::BcXml(BcXml {
                    user_list: Some(user_list),
                    ..
                })),
            ..
        }) = msg.body
        {
            Ok(user_list)
        } else {
            Err(Error::UnintelligibleReply {
                _reply: std::sync::Arc::new(Box::new(msg)),
                why: "Expected ModernMsg payload with a user_list but it was not recieved",
            })
        }
    }

    /// Add a new user
    ///
    /// This function does not check if the user exist and the API will likely throw an error if
    /// that is the case
    pub async fn add_user(
        &self,
        user_name: String,
        password: String,
        user_level: u8,
    ) -> Result<()> {
        let users = self.get_users().await;

        let mut users = users?.user_list.unwrap_or(Vec::new());
        if users.iter().any(|user| user.user_name == user_name) {
            return Err(Error::Other("User already exists"));
        }

        users.push(User {
            user_set_state: "add".to_owned(),
            user_name,
            password: Some(password),
            user_level,
            user_id: None,
            login_state: None,
        });

        self.set_users(users).await
    }

    /// Modify a user. It seems the only property of a user that is modifiable is the password.
    pub async fn modify_user(&self, user_name: String, password: String) -> Result<()> {
        let users = self.get_users().await;

        let mut users = users?.user_list.unwrap_or(Vec::new());

        if let Some(user) = users.iter_mut().find(|user| user.user_name == user_name) {
            user.user_set_state = "modify".to_owned();
            user.password = Some(password.clone());
        } else {
            return Err(Error::Other("User not found"));
        }

        self.set_users(users).await
    }

    /// Remove a user. This does not check for the existence of that user and will likely return an
    /// error if the user doesn't exist.
    pub async fn delete_user(&self, user_name: String) -> Result<()> {
        let users = self.get_users().await;

        let mut users = users?.user_list.unwrap_or(Vec::new());

        if let Some(user) = users.iter_mut().find(|user| user.user_name == user_name) {
            user.user_set_state = "delete".to_owned();
        } else {
            return Err(Error::Other("User not found"));
        }

        self.set_users(users).await
    }

    /// Helper method to send a UserList and wait for its success/failure.
    async fn set_users(&self, users: Vec<User>) -> Result<()> {
        let bcxml = BcXml {
            user_list: Some(UserList {
                version: "1.1".to_owned(),
                user_list: Some(users),
            }),
            ..Default::default()
        };

        let connection = self.get_connection();
        let msg_num = self.new_message_num();
        let mut sub_set = connection
            .subscribe(MSG_ID_UPDATE_USER_LIST, msg_num)
            .await?;

        let get = Bc {
            meta: BcMeta {
                msg_id: MSG_ID_UPDATE_USER_LIST,
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
}
