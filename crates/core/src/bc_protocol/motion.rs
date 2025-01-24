use super::{BcCamera, Error, Result};
use crate::bc::{model::*, xml::*};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::{channel, error::TryRecvError, Receiver};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

/// Motion Events that the callback can send
#[derive(Clone, Copy, Debug)]
pub enum MotionEvents {
    /// Sent when motion is first detected
    Start(Instant),
    /// Sent when motion stops
    Stop(Instant),
    /// Sent when a doorbell is pressed
    Vistor(Instant),
    /// Sent when an AI is recieved
    Ai(Instant, AiKind),
    /// Sent when an Alarm about something other than motion was received
    NoChange(Instant),
}

#[derive(Clone, Copy, Debug)]
/// The kinds of objects the AI can detect
pub enum AiKind {
    /// A person
    Person,
    /// A car
    Car,
    /// Something else, open a PR to fix with actual thing
    Other,
}

/// A handle on current motion related events comming from the camera
///
/// When this object is dropped the motion events are stopped
pub struct MotionData {
    handle: JoinSet<Result<()>>,
    cancel: CancellationToken,
    rx: Receiver<Result<MotionEvents>>,
    history: MotionHistory,
}

pub struct MotionHistory {
    motion: MotionEvents,
    visitor: MotionEvents,
    ai: MotionEvents,
}

impl MotionData {
    /// Get if motion has been detected. Returns None if
    /// no motion data has yet been recieved from the camera
    ///
    /// An error is raised if the motion connection to the camera is dropped
    pub fn motion_detected(&mut self) -> Result<Option<bool>> {
        self.consume_motion_events()?;
        Ok(match &self.history.motion {
            MotionEvents::Start(_) => Some(true),
            MotionEvents::Stop(_) => Some(false),
            _ => None,
        })
    }

    /// Get if motion has been detected within given duration. Returns None if
    /// no motion data has yet been recieved from the camera
    ///
    /// An error is raised if the motion connection to the camera is dropped
    pub fn motion_detected_within(&mut self, duration: Duration) -> Result<Option<bool>> {
        self.consume_motion_events()?;
        Ok(match &self.history.motion {
            MotionEvents::Start(_) => Some(true),
            MotionEvents::Stop(time) => Some((Instant::now() - *time) < duration),
            _ => None,
        })
    }

    /// Consume all the motion events on the queue and update the history
    ///
    /// An error is raised if the motion connection to the camera is dropped
    pub fn consume_motion_events(&mut self) -> Result<()> {
        while self.try_recv()?.is_some() {
            // Tight loop....
            // Should not be many of such messages in the queue though
        }
        Ok(())
    }

    fn update_history(&mut self, latest_event: &MotionEvents) {
        match latest_event {
            MotionEvents::Start(_) => {
                self.history.motion = *latest_event;
            }
            MotionEvents::Stop(_) => {
                self.history.motion = *latest_event;
            }
            MotionEvents::Vistor(_) => {
                self.history.visitor = *latest_event;
            }
            MotionEvents::Ai(_, _) => {
                self.history.ai = *latest_event;
            }
            MotionEvents::NoChange(_) => {}
        }
    }

    /// Try to recv a new motion event on the pipeline
    pub fn try_recv(&mut self) -> Result<Option<MotionEvents>> {
        match self.rx.try_recv() {
            Ok(motion) => {
                let motion = motion?;
                self.update_history(&motion);
                Ok(Some(motion))
            }
            Err(TryRecvError::Empty) => Ok(None),
            Err(e) => Err(Error::from(e)),
        }
    }

    /// Await a new motion event
    pub async fn recv(&mut self) -> Result<Option<MotionEvents>> {
        match self.rx.recv().await {
            Some(motion) => {
                let motion = motion?;
                self.update_history(&motion);
                Ok(Some(motion))
            }
            None => Ok(None),
        }
    }

    /// Wait for the motion to stop
    ///
    /// It must be stopped for at least the given duration
    pub async fn await_stop(&mut self, duration: Duration) -> Result<()> {
        self.consume_motion_events()?;
        let mut last_motion = self.history.motion;
        loop {
            if let MotionEvents::Stop(time) = last_motion {
                // In stop state
                if duration.is_zero() || (Instant::now() - time) > duration {
                    return Ok(());
                } else {
                    // Schedule a sleep or wait for motion to start
                    let remaining_sleep = duration - (Instant::now() - time);
                    let result = tokio::select! {
                        _ = tokio::time::sleep(remaining_sleep) => {None},
                        v = async {
                            loop {
                                match self.recv().await {
                                    n @ Ok(Some(MotionEvents::Start(_))) => {return n;},
                                    n @ Ok(None) => {return n;} // End of data
                                    n @ Err(_) => {return n;},
                                    _ => {continue;}
                                }
                            }
                        } => {Some(v)}
                    };
                    match result {
                        Some(Ok(None)) => {
                            // Camera has dropped motion
                            return Ok(());
                        }
                        Some(Ok(Some(_))) => {
                            // conitnue to next data
                        }
                        Some(Err(e)) => return Err(e),
                        None => {
                            // Timoout occured motion stopped
                            return Ok(());
                        }
                    }
                }
            }
            if let Some(next_motion) = self.recv().await? {
                last_motion = next_motion;
            } else {
                // Camera had dropped motion
                return Ok(());
            }
        }
    }

    /// Wait for the motion to start
    ///
    /// The motion must have a minimum duration as given
    pub async fn await_start(&mut self, duration: Duration) -> Result<()> {
        self.consume_motion_events()?;
        let mut last_motion = self.history.motion;
        loop {
            if let MotionEvents::Start(time) = last_motion {
                // In start state
                if duration.is_zero() || (Instant::now() - time) > duration {
                    return Ok(());
                } else {
                    // Schedule a sleep or wait for motion to stop
                    let result = tokio::select! {
                        _ = tokio::time::sleep(duration - (Instant::now() - time)) => {None},
                        v = async {
                            loop {
                                match self.recv().await {
                                    n @ Ok(Some(MotionEvents::Stop(_))) => {return n;},
                                    n @ Ok(None) => {return n;} // End of data
                                    n @ Err(_) => {return n;},
                                    _ => {continue;}
                                }
                            }
                        } => {Some(v)}
                    };
                    match result {
                        Some(Ok(None)) => {
                            // Camera has dropped motion
                            return Ok(());
                        }
                        Some(Ok(Some(_))) => {
                            // conitnue to next data
                        }
                        Some(Err(e)) => return Err(e),
                        None => {
                            // Timeout occured motion started
                            return Ok(());
                        }
                    }
                }
            }
            if let Some(next_motion) = self.recv().await? {
                last_motion = next_motion;
            } else {
                // Camera had dropped motion
                return Ok(());
            }
        }
    }
}

impl BcCamera {
    /// This message tells the camera to send the motion events to us
    /// Which are the recieved on msgid 33
    async fn start_motion_query(&self) -> Result<u16> {
        self.has_ability_rw("motion").await?;
        let connection = self.get_connection();

        let msg_num = self.new_message_num();
        let mut sub = connection.subscribe(MSG_ID_MOTION_REQUEST, msg_num).await?;
        let msg = Bc {
            meta: BcMeta {
                msg_id: MSG_ID_MOTION_REQUEST,
                channel_id: self.channel_id,
                msg_num,
                stream_type: 0,
                response_code: 0,
                class: 0x6414,
            },
            body: BcBody::ModernMsg(ModernMsg {
                ..Default::default()
            }),
        };

        sub.send(msg).await?;

        let msg = sub.recv().await?;

        if let BcMeta {
            response_code: 200, ..
        } = msg.meta
        {
            Ok(msg_num)
        } else {
            Err(Error::UnintelligibleReply {
                reply: std::sync::Arc::new(Box::new(msg)),
                why: "The camera did not accept the request to start motion",
            })
        }
    }

    /// This returns a data structure which can be used to
    /// query motion events
    pub async fn listen_on_motion(&self) -> Result<MotionData> {
        self.start_motion_query().await?;

        let connection = self.get_connection();

        // After start_motion_query (MSG_ID 31) the camera sends motion messages
        // when whenever motion is detected.
        let (tx, rx) = channel(20);

        let mut set = JoinSet::new();
        let channel_id = self.channel_id;
        let cancel = CancellationToken::new();
        let thread_cancel = cancel.clone();
        set.spawn(async move {
            tokio::select! {
                _ = thread_cancel.cancelled() => Result::Ok(()),
                v = async {
                    let mut sub = connection.subscribe_to_id(MSG_ID_MOTION).await?;

                    loop {
                        tokio::task::yield_now().await;
                        let msg = sub.recv().await;
                        let status_list = match msg {
                            Ok(motion_msg) => {
                                if let BcBody::ModernMsg(ModernMsg {
                                    payload:
                                        Some(BcPayloads::BcXml(BcXml {
                                            alarm_event_list: Some(alarm_event_list),
                                            ..
                                        })),
                                    ..
                                }) = motion_msg.body
                                {
                                    let mut result = vec![];
                                    for alarm_event in &alarm_event_list.alarm_events {
                                        if alarm_event.channel_id == channel_id {
                                            if alarm_event.status == "visitor" {
                                                result.push(MotionEvents::Vistor(Instant::now()));
                                            } else if alarm_event.status == "MD" {
                                                result.push(MotionEvents::Start(Instant::now()));
                                            } else if alarm_event.ai_type != "none" {
                                                result.push(MotionEvents::Ai(Instant::now(),
                                                    match alarm_event.ai_type.as_ref() {
                                                        "person" | "people" | "human" => AiKind::Person,
                                                        "car" | "vehicle" => AiKind::Car,
                                                        _ => AiKind::Other,
                                                    }
                                                ));
                                            } else {
                                                result.push(MotionEvents::Stop(Instant::now()));
                                            }
                                        }
                                    }
                                    Ok(result)
                                } else {
                                    Ok(vec![])
                                }
                            }
                            // On connection drop we stop
                            Err(e) => Err(e),
                        };

                        match status_list {
                            Ok(mut status_list) => {
                                for status in status_list.drain(..) {
                                    if tx.send(Ok(status)).await.is_err() {
                                        // Motion reciever has been dropped
                                        break;
                                    }
                                }
                            },
                            Err(e) => {
                                // Err from camera
                                if tx.send(Err(e)).await.is_err() {
                                    // Motion reciever has been dropped
                                }
                                break;
                            }
                        }
                    }
                    Ok(())
                } => v,
            }
        });

        Ok(MotionData {
            handle: set,
            cancel,
            rx,
            history: MotionHistory {
                motion: MotionEvents::NoChange(Instant::now()),
                visitor: MotionEvents::NoChange(Instant::now()),
                ai: MotionEvents::NoChange(Instant::now()),
            },
        })
    }
}

impl Drop for MotionData {
    fn drop(&mut self) {
        log::trace!("Drop MotionData");
        self.cancel.cancel();
        let mut handle = std::mem::take(&mut self.handle);
        let _gt = tokio::runtime::Handle::current().enter();
        tokio::task::spawn(async move {
            while handle.join_next().await.is_some() {}
            log::trace!("Dropped MotionData");
        });
    }
}
