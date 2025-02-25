// Copyright (c) Sean Lawlor
//
// This source code is licensed under both the MIT license found in the
// LICENSE-MIT file in the root directory of this source tree.

//! Test the synchronization of paging groups (and inherently the pid registry).
//!
//! This test creates a "PingPong" actor on both remote nodes and joins them to the
//! "test" pg group. The framework synchronizes the PG groups together and then
//! the test starts with one ping pong sending "Ping" to its peer in the remote system.
//! Upon receiving a ping, a node will take the non-local PG members in their own group
//! and send "Ping" back. This should bounce between nodes in pair until 10 bounces occur
//! then both actors set their internal state to "done = true".
//!
//! The test completes, when the outer test case detects the local ping actor has completed,
//! it then does a teardown

use clap::Args;
use ractor::concurrency::{sleep, Duration, Instant};
use ractor::{Actor, ActorProcessingErr, ActorRef, RpcReplyPort};
use ractor_cluster::RactorClusterMessage;

const NUM_PING_PONGS: u16 = 10;
const PING_PONG_ALLOTED_MS: u128 = 1500;

struct PingPongActor;

#[derive(RactorClusterMessage)]
enum PingPongActorMessage {
    Ping,
    #[rpc]
    IsDone(RpcReplyPort<bool>),
}

struct PingPongActorState {
    count: u16,
    done: bool,
}

#[async_trait::async_trait]
impl Actor for PingPongActor {
    type Msg = PingPongActorMessage;
    type State = PingPongActorState;
    type Arguments = ();

    async fn pre_start(
        &self,
        myself: ActorRef<Self>,
        _: (),
    ) -> Result<Self::State, ActorProcessingErr> {
        ractor::pg::join("test".to_string(), vec![myself.get_cell()]);
        Ok(PingPongActorState {
            count: 0,
            done: false,
        })
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        let group = "test".to_string();
        let remote_actors = ractor::pg::get_members(&group)
            .into_iter()
            .filter(|actor| !actor.get_id().is_local())
            .map(ActorRef::<Self>::from)
            .collect::<Vec<_>>();
        match message {
            Self::Msg::Ping => {
                log::info!(
                    "Received a ping, replying in kind to {} remote actors",
                    remote_actors.len()
                );
                for act in remote_actors {
                    act.cast(PingPongActorMessage::Ping)?;
                }
                state.count += 1;

                if state.count > NUM_PING_PONGS {
                    state.done = true;
                }
            }
            Self::Msg::IsDone(reply) => {
                let _ = reply.send(state.done);
            }
        }

        Ok(())
    }
}

/// Configuration
#[derive(Args, Debug, Clone)]
pub struct PgGroupsConfig {
    /// Server port
    server_port: u16,
    /// If specified, represents the client to connect to
    client_port: Option<u16>,
    /// If specified, represents the client to connect to
    client_host: Option<String>,
}

pub(crate) async fn test(config: PgGroupsConfig) -> i32 {
    let cookie = "cookie".to_string();
    let hostname = "localhost".to_string();

    let server =
        ractor_cluster::NodeServer::new(config.server_port, cookie, super::random_name(), hostname);

    let (actor, handle) = Actor::spawn(None, server, ())
        .await
        .expect("Failed to start NodeServer A");

    let (test_actor, test_handle) = Actor::spawn(None, PingPongActor, ())
        .await
        .expect("Ping pong actor failed to start up!");

    if let (Some(client_host), Some(client_port)) = (config.client_host, config.client_port) {
        log::info!(
            "Connecting to remote NodeServer at {}:{}",
            client_host,
            client_port
        );
        if let Err(error) =
            ractor_cluster::node::client::connect(&actor, format!("{client_host}:{client_port}"))
                .await
        {
            log::error!("Failed to connect with error {error}");
            return -3;
        } else {
            log::info!("Client connected NodeServer b to NodeServer a");
        }
    }

    let mut err_code = -1;
    log::info!("Waiting for NodeSession status updates");

    let mut rpc_reply = ractor::call_t!(actor, ractor_cluster::NodeServerMessage::GetSessions, 200);
    while rpc_reply.is_ok() {
        if let Some(item) = rpc_reply
            .unwrap()
            .into_values()
            .collect::<Vec<_>>()
            .first()
            .cloned()
        {
            let is_authenticated = ractor::call_t!(
                item,
                ractor_cluster::NodeSessionMessage::GetAuthenticationState,
                200
            );
            match is_authenticated {
                Err(err) => {
                    log::warn!("NodeSession returned error on rpc query {}", err);
                    break;
                }
                Ok(false) => {
                    // Still waiting
                }
                Ok(true) => {
                    err_code = 0;
                    log::info!("Authentication succeeded. Exiting test");
                    break;
                }
            }
        }
        // try again
        rpc_reply = ractor::call_t!(actor, ractor_cluster::NodeServerMessage::GetSessions, 200);
    }

    if err_code == 0 {
        // we're authenticated. Startup our PG group testing
        let _ = ractor::cast!(test_actor, PingPongActorMessage::Ping);
        let tic = Instant::now();

        let mut rpc_result = ractor::call_t!(test_actor, PingPongActorMessage::IsDone, 500);
        while rpc_result.is_ok() {
            let duration: Duration = Instant::now() - tic;
            if duration.as_millis() > PING_PONG_ALLOTED_MS {
                log::error!("Ping pong actor didn't complete in allotted time");
                return -1;
            }

            match rpc_result {
                Ok(true) => {
                    // test completed
                    log::info!("Ping pong actor is completed");
                    break;
                }
                Ok(_) => {
                    // test still WIP
                }
                Err(err) => {
                    log::error!(
                        "Failed to communicate with test actor or messaging timeout '{}'",
                        err
                    );
                    return -4;
                }
            }
            rpc_result = ractor::call_t!(test_actor, PingPongActorMessage::IsDone, 500);
        }
    } else {
        log::warn!("Failed to authenticate, failing test");
    }

    log::info!("Terminating test - code {}", err_code);

    sleep(Duration::from_millis(250)).await;

    // cleanup
    test_actor.stop(None);
    test_handle.await.unwrap();

    actor.stop(None);
    handle.await.unwrap();

    err_code
}
