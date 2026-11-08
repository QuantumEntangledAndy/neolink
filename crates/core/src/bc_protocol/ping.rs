use super::{BcCamera, BcConnection, Result};
use crate::bc::model::*;
use std::sync::{
    atomic::{AtomicU16, Ordering},
    Arc, Weak,
};
use std::time::Duration;

fn ping_msg(channel_id: u8, msg_num: u16) -> Bc {
    Bc {
        meta: BcMeta {
            msg_id: MSG_ID_PING,
            channel_id,
            msg_num,
            stream_type: 0,
            response_code: 0,
            class: 0x6414,
        },
        body: BcBody::ModernMsg(ModernMsg {
            ..Default::default()
        }),
    }
}

impl BcCamera {
    /// Ping the camera will either return Ok(()) which means a sucess reply
    /// or error
    pub async fn ping(&self) -> Result<()> {
        let connection = self.get_connection();
        let msg_num = self.new_message_num();
        let mut sub_ping = connection.subscribe(MSG_ID_PING, msg_num).await?;

        sub_ping.send(ping_msg(self.channel_id, msg_num)).await?;

        sub_ping.recv().await?;

        log::trace!("Ping complete");
        Ok(())
    }

    /// Send a fire-and-forget ping every `interval` for as long as the
    /// connection is alive. Replies are not awaited.
    ///
    /// Some TCP cameras (e.g. the Reolink B500) stop sending stream data
    /// after the first frame unless they are pinged roughly once a second.
    pub(crate) fn start_tcp_ping(&self, interval: Duration) {
        spawn_tcp_ping(
            Arc::downgrade(&self.connection),
            self.channel_id,
            self.message_num.clone(),
            interval,
        );
    }
}

/// Only a weak reference is held so the task never keeps the connection
/// (and the camera's TCP session) open after the camera is dropped.
fn spawn_tcp_ping(
    connection: Weak<BcConnection>,
    channel_id: u8,
    message_num: Arc<AtomicU16>,
    interval: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(interval);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // The first tick completes immediately; the login exchange has just
        // happened so there is no need to ping straight away.
        tick.tick().await;
        loop {
            tick.tick().await;
            let Some(connection) = connection.upgrade() else {
                break;
            };
            let msg_num = message_num.fetch_add(1, Ordering::Relaxed);
            if connection
                .send(ping_msg(channel_id, msg_num))
                .await
                .is_err()
            {
                break;
            }
        }
        log::trace!("TCP ping task stopped");
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bc::crypto::EncryptionProtocol;
    use crate::Error;
    use futures::SinkExt;
    use tokio::sync::mpsc::{channel, Receiver};
    use tokio_util::sync::PollSender;

    async fn fake_connection() -> (BcConnection, Receiver<Bc>) {
        let (tx, rx) = channel::<Bc>(100);
        let sink = PollSender::new(tx).sink_map_err(Error::from);
        let source = futures::stream::pending::<Result<Bc>>();
        let conn = BcConnection::new(Box::new(sink), Box::new(source))
            .await
            .unwrap();
        (conn, rx)
    }

    #[test]
    fn test_ping_msg_header() {
        let msg = ping_msg(0, 7);
        let buf = msg
            .serialize(vec![], &EncryptionProtocol::Unencrypted)
            .unwrap();
        // 24 byte modern header with an empty body
        assert_eq!(buf.len(), 24);
        assert_eq!(&buf[0..4], &[0xf0, 0xde, 0xbc, 0x0a]); // magic
        assert_eq!(&buf[4..8], &93u32.to_le_bytes()); // msg_id
        assert_eq!(&buf[8..12], &0u32.to_le_bytes()); // body length
        assert_eq!(buf[12], 0); // channel_id
        assert_eq!(&buf[14..16], &7u16.to_le_bytes()); // msg_num
        assert_eq!(&buf[16..18], &0u16.to_le_bytes()); // response code
        assert_eq!(&buf[18..20], &0x6414u16.to_le_bytes()); // class
        assert_eq!(&buf[20..24], &0u32.to_le_bytes()); // payload offset
    }

    #[tokio::test]
    async fn test_tcp_ping_sends_periodically() {
        let (conn, mut rx) = fake_connection().await;
        let conn = Arc::new(conn);
        let message_num = Arc::new(AtomicU16::new(5));
        let task = spawn_tcp_ping(
            Arc::downgrade(&conn),
            3,
            message_num.clone(),
            Duration::from_millis(20),
        );

        let mut nums = vec![];
        for _ in 0..3 {
            let bc = tokio::time::timeout(Duration::from_secs(2), rx.recv())
                .await
                .expect("ping not sent in time")
                .expect("sink closed");
            assert_eq!(bc.meta.msg_id, MSG_ID_PING);
            assert_eq!(bc.meta.channel_id, 3);
            assert_eq!(bc.meta.class, 0x6414);
            nums.push(bc.meta.msg_num);
        }
        // Shares the camera's message counter so numbers never collide
        assert_eq!(nums, vec![5, 6, 7]);
        assert!(message_num.load(Ordering::Relaxed) >= 8);
        task.abort();
    }

    #[tokio::test]
    async fn test_tcp_ping_stops_when_connection_dropped() {
        let (conn, _rx) = fake_connection().await;
        let conn = Arc::new(conn);
        let task = spawn_tcp_ping(
            Arc::downgrade(&conn),
            0,
            Arc::new(AtomicU16::new(0)),
            Duration::from_millis(10),
        );
        drop(conn);
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("ping task kept running after the connection was dropped")
            .unwrap();
    }
}
