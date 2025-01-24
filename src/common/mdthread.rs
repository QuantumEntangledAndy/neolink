//! This thread will listen to motion messages
//! from the camera.

use anyhow::Context;
use std::sync::Arc;
use tokio::{
    sync::{
        mpsc::Receiver as MpscReceiver,
        oneshot::Sender as OneshotSender,
        watch::{channel as watch, Receiver as WatchReceiver, Sender as WatchSender},
    },
    time::{sleep, Duration, Instant},
};
use tokio_util::sync::CancellationToken;

use super::NeoInstance;
use crate::{AnyResult, Result};
use neolink_core::bc_protocol::{AiKind, MotionEvents};

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(crate) enum MdState {
    Start(Instant),
    Stop(Instant),
    Unknown,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(crate) enum VisitorState {
    Visted(Instant),
    Unknown,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(crate) enum AiState {
    Person(Instant),
    Car(Instant),
    Other(Instant),
    Unknown,
}

pub(crate) struct NeoCamMdThread {
    md_watcher: Arc<WatchSender<MdState>>,
    visitor_watcher: Arc<WatchSender<VisitorState>>,
    ai_watcher: Arc<WatchSender<AiState>>,
    md_request_rx: MpscReceiver<MdRequest>,
    cancel: CancellationToken,
    instance: NeoInstance,
}

impl NeoCamMdThread {
    pub(crate) async fn new(
        md_request_rx: MpscReceiver<MdRequest>,
        instance: NeoInstance,
    ) -> Result<Self> {
        let (md_watcher, _) = watch(MdState::Unknown);
        let md_watcher = Arc::new(md_watcher);

        let (visitor_watcher, _) = watch(VisitorState::Unknown);
        let visitor_watcher = Arc::new(visitor_watcher);

        let (ai_watcher, _) = watch(AiState::Unknown);
        let ai_watcher = Arc::new(ai_watcher);

        Ok(Self {
            md_watcher,
            visitor_watcher,
            ai_watcher,
            md_request_rx,
            cancel: CancellationToken::new(),
            instance,
        })
    }

    pub(crate) async fn run(&mut self) -> Result<()> {
        let thread_cancel = self.cancel.clone();
        let watcher = self.md_watcher.clone();
        let visitor_watcher = self.visitor_watcher.clone();
        let ai_watcher = self.ai_watcher.clone();
        let md_instance = self.instance.clone();
        tokio::select! {
            _ = thread_cancel.cancelled() => {
                Ok(())
            },
            v = async {
                while let Some(request) = self.md_request_rx.recv().await {
                    match request {
                        MdRequest::Md {
                            sender
                        } => {
                          let _ = sender.send(self.md_watcher.subscribe());
                        },
                        MdRequest::Visitor {
                            sender
                        } => {
                          let _ = sender.send(self.visitor_watcher.subscribe());
                        },
                        MdRequest::Ai {
                            sender
                        } => {
                          let _ = sender.send(self.ai_watcher.subscribe());
                        },
                    }
                }
                Ok(())
            } => v,
            v = async {
                loop {
                    let r: AnyResult<()> = md_instance.run_passive_task(|cam| {
                        let watcher = watcher.clone();
                        let visitor_watcher = visitor_watcher.clone();
                        let ai_watcher = ai_watcher.clone();
                        Box::pin(
                        async move {
                            let mut md = cam.listen_on_motion().await.with_context(|| "Error in getting MD listen_on_motion")?;
                            loop {
                                let event = md.recv().await.with_context(|| "Error in getting MD next_motion")?.ok_or(anyhow::anyhow!("MD connection was dropped"))?;
                                match event {
                                    MotionEvents::Start(at) => {
                                        watcher.send_replace(
                                            MdState::Start(at.into())
                                        );
                                    }
                                    MotionEvents::Stop(at) => {
                                        watcher.send_replace(
                                            MdState::Stop(at.into())
                                        );
                                    },
                                    MotionEvents::Vistor(at) => {
                                        visitor_watcher.send_replace(
                                            VisitorState::Visted(at.into())
                                        );
                                    },
                                    MotionEvents::Ai(at, kind) => {
                                        ai_watcher.send_replace(
                                            match kind {
                                                AiKind::Person => {
                                                    AiState::Person(at.into())
                                                }
                                                AiKind::Car => {
                                                    AiState::Car(at.into())
                                                }
                                                AiKind::Other => {
                                                    AiState::Other(at.into())
                                                }
                                            }

                                        );
                                    },
                                    _ => {},
                                }
                            }
                        }
                    )}).await;
                    log::debug!("Error in MD task Restarting: {:?}", r);
                    sleep(Duration::from_secs(1)).await;
                }
            } => v
        }
    }
}

impl Drop for NeoCamMdThread {
    fn drop(&mut self) {
        log::trace!("Drop NeoCamMdThread");
        self.cancel.cancel();
        log::trace!("Dropped NeoCamMdThread");
    }
}

/// Used to pass messages to the MdThread
pub(crate) enum MdRequest {
    Md {
        sender: OneshotSender<WatchReceiver<MdState>>,
    },
    Visitor {
        sender: OneshotSender<WatchReceiver<VisitorState>>,
    },
    Ai {
        sender: OneshotSender<WatchReceiver<AiState>>,
    },
}
