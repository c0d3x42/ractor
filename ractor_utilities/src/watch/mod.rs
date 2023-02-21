// This source code is licensed under both the MIT license found in the
// LICENSE-MIT file in the root directory of this source tree.

use std::{collections::HashMap, path::{Path }, sync::Arc};
use futures::{channel::mpsc::channel, SinkExt, StreamExt};
use notify::{
    event::{AccessKind, AccessMode},
    EventKind, RecommendedWatcher, Watcher, Event, Error as NotifyError
};

use ractor::{Actor, ActorRef, OutputPort, RpcReplyPort, Message};
use tokio::task::JoinHandle;

#[allow(dead_code)]
pub enum WatchMessage {
    Start,
    Stop,
    Watch(String),
    WatchPort(String, RpcReplyPort<Arc<OutputPort<String>>>),
    WatchNotification(String),
}
impl Message for WatchMessage{}

pub struct Watch { }

#[allow(dead_code)]
pub struct WatchState {
    notify_watcher: RecommendedWatcher,
    //receiver: FutReceiver<notify::Result<notify::Event>>
    notify_handle: JoinHandle<()>,

    path_ports: HashMap<String, Arc<OutputPort<String>>>,
}


#[async_trait::async_trait]
impl Actor for Watch {
    type Arguments = ();
    type Msg = WatchMessage;
    type State = WatchState;

    async fn pre_start(
        &self,
        myself: ractor::ActorRef<Self>,
        _args: Self::Arguments,
    ) -> Result<Self::State, ractor::ActorProcessingErr> {
        let (mut tx, mut receiver) = channel::<Result<Event, NotifyError>>(1);

        log::debug!("Starting DirWatcher");
        let notify_watcher = RecommendedWatcher::new(
            move |res| {
                futures::executor::block_on(async {
                    tx.send(res).await.expect("to send a watched notification");
                })
            },
            notify::Config::default(),
        )?;

        log::debug!("Started DirWatcher thread");
        let cloned_myself = myself.clone();

        let notify_handle = tokio::spawn(async move {
            log::debug!("Spawning tokio receiver");
            while let Some(res) = receiver.next().await {
                match res {
                    Ok(event) => {
                        log::debug!("received a watcher event {:?}", event.kind);
                        match event.kind {
                            EventKind::Access(AccessKind::Close(AccessMode::Write)) => {
                                for pb in event.paths {
                                    log::debug!("Path: {:?}", pb);

                                    let watched_path = pb.as_path().display().to_string();
                                    log::debug!("WatchedPath = [{watched_path}]");

                                    let _ = cloned_myself
                                        .send_message(WatchMessage::WatchNotification(watched_path));
                                }
                            }
                            _ => {}
                        }
                    }
                    Err(err) => todo!("err receiver: {err}"),
                }
            }
        });

        Ok(WatchState {
            notify_watcher,
            notify_handle,
            path_ports: HashMap::new(),
        })
    }

    async fn handle_supervisor_evt(
        &self,
        _myself: ActorRef<Self>,
        message: ractor::SupervisionEvent,
        _state: &mut Self::State,
    ) -> Result<(), ractor::ActorProcessingErr> {
        log::error!("Supervisor: {message}");

        Ok(())
    }

    async fn handle(
        &self,
        _myself: ractor::ActorRef<Self>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ractor::ActorProcessingErr> {
        match message {
            WatchMessage::WatchPort(p, reply) => {
                log::debug!("WatchPort on {p}");

                let path = Path::new(&p);
                if let Err(err) = state.notify_watcher.watch(&path, notify::RecursiveMode::Recursive) {
                    log::error!("Failed to start watching on {p}: {err}");
                }

                log::debug!("Watching: {p}");

                let port = Arc::new(OutputPort::default());

                if !reply.is_closed() {
                    if let Err(err) = reply.send(port.clone()) {
                        log::error!("Failed to send WatchPort: {err}");
                    }
                } else {
                    log::warn!("WatchPort is closed for {p}");
                }

                // seems that the watcher will emit absolute paths,
                // and because we are observing relatives paths, the subsequent comparisons
                // for path.starts_with will fail. so we prepend with with a full path

                state.path_ports.insert(p, port);
            }
            WatchMessage::WatchNotification(watched_abs_path) => {
                // seems to be an absolute path...

                log::info!("Watched Path has changed: {watched_abs_path}");

                for (interested_path, output_port) in &state.path_ports {
                    log::debug!("comparing path_port [{watched_abs_path}] starts with {interested_path}");

                    let abspath = Path::new(&watched_abs_path);
                    let cwd = std::env::current_dir().unwrap().display().to_string();

                    // strip the CWD from the notified path
                    let relpath = abspath.strip_prefix(cwd).unwrap();

                    log::debug!("comparing relpath path_port [{}] starts with {interested_path}", relpath.display());
                    if relpath.starts_with(interested_path) {
                        output_port.send(relpath.display().to_string());
                    }
                }
            }
            _ => todo!("other watch message types"),
        }

        Ok(())
    }
}