// Copyright (c) Sean Lawlor
//
// This source code is licensed under both the MIT license found in the
// LICENSE-MIT file in the root directory of this source tree.

//! This module contains the basic building blocks of an actor.
//!
//! They are:
//! [Actor]: The behavior definition for an actor's internal processing logic + state management
//! [Actor]: Management structure processing the message handler, signals, and supervision events in a loop

use std::panic::AssertUnwindSafe;

use futures::TryFutureExt;

use crate::concurrency::JoinHandle;
#[cfg(feature = "cluster")]
use crate::ActorId;

pub mod messages;
use messages::*;

pub mod actor_cell;
pub mod errors;
pub mod supervision;

#[cfg(test)]
mod tests;

use crate::{ActorName, Message, State};
use actor_cell::{ActorCell, ActorPortSet, ActorRef, ActorStatus};
use errors::{ActorErr, ActorProcessingErr, MessagingErr, SpawnErr};

pub(crate) fn get_panic_string(e: Box<dyn std::any::Any + Send>) -> ActorProcessingErr {
    match e.downcast::<String>() {
        Ok(v) => From::from(*v),
        Err(e) => match e.downcast::<&str>() {
            Ok(v) => From::from(*v),
            _ => From::from("Unknown panic occurred which couldn't be coerced to a string"),
        },
    }
}

/// [Actor] defines the behavior of an Actor. It specifies the
/// Message type, State type, and all processing logic for the actor
///
/// Additionally it aliases the calls for `spawn` and `spawn_linked` from
/// [ActorRuntime] for convenient startup + lifecycle management
///
/// NOTE: All of the implemented trait functions
///
/// * `pre_start`
/// * `post_start`
/// * `post_stop`
/// * `handle`
/// * `handle_serialized` (Available with `cluster` feature only)
/// * `handle_supervision_evt`
///
/// return a [Result<_, ActorProcessingError>] where the error type is an
/// alias of [Box<dyn std::error::Error + Send + Sync + 'static>]. This is treated
/// as an "unhandled" error and will terminate the actor + execute necessary supervision
/// patterns. Panics are also captured from the inner functions and wrapped into an Error
/// type, however should an [Err(_)] result from any of these functions the **actor will
/// terminate** and cleanup.
#[async_trait::async_trait]
pub trait Actor: Sized + Sync + Send + 'static {
    /// The message type for this actor
    type Msg: Message;

    /// The type of state this actor manages internally
    type State: State;

    /// Initialization arguments
    type Arguments: State;

    /// Invoked when an actor is being started by the system.
    ///
    /// Any initialization inherent to the actor's role should be
    /// performed here hence why it returns the initial state.
    ///
    /// Panics in `pre_start` do not invoke the
    /// supervision strategy and the actor won't be started. [Actor]::`spawn`
    /// will return an error to the caller
    ///
    /// * `myself` - A handle to the [ActorCell] representing this actor
    /// * `args` - Arguments that are passed in the spawning of the actor which might
    /// be necessary to construct the initial state
    ///
    /// Returns an initial [Actor::State] to bootstrap the actor
    async fn pre_start(
        &self,
        myself: ActorRef<Self>,
        args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr>;

    /// Invoked after an actor has started.
    ///
    /// Any post initialization can be performed here, such as writing
    /// to a log file, emitting metrics.
    ///
    /// Panics in `post_start` follow the supervision strategy.
    ///
    /// * `myself` - A handle to the [ActorCell] representing this actor
    /// * `state` - A mutable reference to the internal actor's state
    #[allow(unused_variables)]
    async fn post_start(
        &self,
        myself: ActorRef<Self>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        Ok(())
    }

    /// Invoked after an actor has been stopped to perform final cleanup. In the
    /// event the actor is terminated with [Signal::Kill] or has self-panicked,
    /// `post_stop` won't be called.
    ///
    /// Panics in `post_stop` follow the supervision strategy.
    ///
    /// * `myself` - A handle to the [ActorCell] representing this actor
    /// * `state` - A mutable reference to the internal actor's last known state
    #[allow(unused_variables)]
    async fn post_stop(
        &self,
        myself: ActorRef<Self>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        Ok(())
    }

    /// Handle the incoming message from the event processing loop. Unhandled panickes will be
    /// captured and sent to the supervisor(s)
    ///
    /// * `myself` - A handle to the [ActorCell] representing this actor
    /// * `message` - The message to process
    /// * `state` - A mutable reference to the internal actor's state
    #[allow(unused_variables)]
    async fn handle(
        &self,
        myself: ActorRef<Self>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        Ok(())
    }

    /// Handle the remote incoming message from the event processing loop. Unhandled panickes will be
    /// captured and sent to the supervisor(s)
    ///
    /// * `myself` - A handle to the [ActorCell] representing this actor
    /// * `message` - The serialized message to handle
    /// * `state` - A mutable reference to the internal actor's state
    #[allow(unused_variables)]
    #[cfg(feature = "cluster")]
    async fn handle_serialized(
        &self,
        myself: ActorRef<Self>,
        message: crate::message::SerializedMessage,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        Ok(())
    }

    /// Handle the incoming supervision event. Unhandled panicks will captured and
    /// sent the the supervisor(s). The default supervision behavior is to ignore all
    /// child events. To override this behavior, implement this method.
    ///
    /// * `myself` - A handle to the [ActorCell] representing this actor
    /// * `message` - The message to process
    /// * `state` - A mutable reference to the internal actor's state
    #[allow(unused_variables)]
    async fn handle_supervisor_evt(
        &self,
        myself: ActorRef<Self>,
        message: SupervisionEvent,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        Ok(())
    }

    /// Spawn an actor of this type, which is unsupervised, automatically starting
    ///
    /// * `name`: A name to give the actor. Useful for global referencing or debug printing
    /// * `handler` The implementation of Self
    /// * `startup_args`: Arguments passed to the `pre_start` call of the [Actor] to facilitate startup and
    /// initial state creation
    ///
    /// Returns a [Ok((ActorRef, JoinHandle<()>))] upon successful start, denoting the actor reference
    /// along with the join handle which will complete when the actor terminates. Returns [Err(SpawnErr)] if
    /// the actor failed to start
    async fn spawn(
        name: Option<ActorName>,
        handler: Self,
        startup_args: Self::Arguments,
    ) -> Result<(ActorRef<Self>, JoinHandle<()>), SpawnErr> {
        ActorRuntime::<Self>::spawn(name, handler, startup_args).await
    }

    /// Spawn an actor of this type with a supervisor, automatically starting the actor
    ///
    /// * `name`: A name to give the actor. Useful for global referencing or debug printing
    /// * `handler` The implementation of Self
    /// * `startup_args`: Arguments passed to the `pre_start` call of the [Actor] to facilitate startup and
    /// initial state creation
    /// * `supervisor`: The [ActorCell] which is to become the supervisor (parent) of this actor
    ///
    /// Returns a [Ok((ActorRef, JoinHandle<()>))] upon successful start, denoting the actor reference
    /// along with the join handle which will complete when the actor terminates. Returns [Err(SpawnErr)] if
    /// the actor failed to start
    async fn spawn_linked(
        name: Option<ActorName>,
        handler: Self,
        startup_args: Self::Arguments,
        supervisor: ActorCell,
    ) -> Result<(ActorRef<Self>, JoinHandle<()>), SpawnErr> {
        ActorRuntime::<Self>::spawn_linked(name, handler, startup_args, supervisor).await
    }
}

/// [ActorRuntime] is a struct which represents the actor. This struct is consumed by the
/// `start` operation, but results in an [ActorRef] to communicate and operate with
pub struct ActorRuntime<TActor>
where
    TActor: Actor,
{
    actor_ref: ActorRef<TActor>,
    handler: TActor,
}

impl<TActor> ActorRuntime<TActor>
where
    TActor: Actor,
{
    /// Spawn an actor, which is unsupervised, automatically starting the actor
    ///
    /// * `name`: A name to give the actor. Useful for global referencing or debug printing
    /// * `handler` The [Actor] defining the logic for this actor
    /// * `startup_args`: Arguments passed to the `pre_start` call of the [Actor] to facilitate startup and
    /// initial state creation
    ///
    /// Returns a [Ok((ActorRef, JoinHandle<()>))] upon successful start, denoting the actor reference
    /// along with the join handle which will complete when the actor terminates. Returns [Err(SpawnErr)] if
    /// the actor failed to start
    pub async fn spawn(
        name: Option<ActorName>,
        handler: TActor,
        startup_args: TActor::Arguments,
    ) -> Result<(ActorRef<TActor>, JoinHandle<()>), SpawnErr> {
        let (actor, ports) = Self::new(name, handler)?;
        actor.start(ports, startup_args, None).await
    }

    /// Spawn an actor with a supervisor, automatically starting the actor
    ///
    /// * `name`: A name to give the actor. Useful for global referencing or debug printing
    /// * `handler` The [Actor] defining the logic for this actor
    /// * `startup_args`: Arguments passed to the `pre_start` call of the [Actor] to facilitate startup and
    /// initial state creation
    /// * `supervisor`: The [ActorCell] which is to become the supervisor (parent) of this actor
    ///
    /// Returns a [Ok((ActorRef, JoinHandle<()>))] upon successful start, denoting the actor reference
    /// along with the join handle which will complete when the actor terminates. Returns [Err(SpawnErr)] if
    /// the actor failed to start
    pub async fn spawn_linked(
        name: Option<ActorName>,
        handler: TActor,
        startup_args: TActor::Arguments,
        supervisor: ActorCell,
    ) -> Result<(ActorRef<TActor>, JoinHandle<()>), SpawnErr> {
        let (actor, ports) = Self::new(name, handler)?;
        actor.start(ports, startup_args, Some(supervisor)).await
    }

    /// Spawn an actor instantly, not waiting on the actor's `pre_start` routine. This is helpful
    /// for actors where you want access to the send messages into the actor's message queue
    /// without waiting on an asynchronous context.
    ///
    /// **WARNING** Failures in the pre_start routine need to be waited on in the join handle
    /// since they will NOT fail the spawn operation in this context
    ///
    /// * `name`: A name to give the actor. Useful for global referencing or debug printing
    /// * `handler` The [Actor] defining the logic for this actor
    /// * `startup_args`: Arguments passed to the `pre_start` call of the [Actor] to facilitate startup and
    /// initial state creation
    ///
    /// Returns a [Ok((ActorRef, JoinHandle<Result<JoinHandle<()>, SpawnErr>>))] upon successful creation of the
    /// message queues, so you can begin sending messages. However the associated [JoinHandle] contains the inner
    /// information around if the actor successfully started or not in it's `pre_start` routine. Returns [Err(SpawnErr)] if
    /// the actor name is already allocated
    #[allow(clippy::type_complexity)]
    pub fn spawn_instant(
        name: Option<ActorName>,
        handler: TActor,
        startup_args: TActor::Arguments,
    ) -> Result<
        (
            ActorRef<TActor>,
            JoinHandle<Result<JoinHandle<()>, SpawnErr>>,
        ),
        SpawnErr,
    > {
        let (actor, ports) = Self::new(name, handler)?;
        let actor_ref = actor.actor_ref.clone();
        let join_op = crate::concurrency::spawn(async move {
            let (_, handle) = actor.start(ports, startup_args, None).await?;
            Ok(handle)
        });
        Ok((actor_ref, join_op))
    }

    /// Spawn an actor instantly with supervision, not waiting on the actor's `pre_start` routine.
    /// This is helpful for actors where you want access to the send messages into the actor's
    /// message queue without waiting on an asynchronous context.
    ///
    /// **WARNING** Failures in the pre_start routine need to be waited on in the join handle
    /// since they will NOT fail the spawn operation in this context. Additionally the supervision
    /// tree will **NOT** be linked until the `pre_start` completes so there is a chance an actor
    /// is lost during `pre_start` and not successfully started unless it's specifically handled
    /// by the caller by awaiting later.
    ///
    /// * `name`: A name to give the actor. Useful for global referencing or debug printing
    /// * `handler` The [Actor] defining the logic for this actor
    /// * `startup_args`: Arguments passed to the `pre_start` call of the [Actor] to facilitate startup and
    /// initial state creation
    /// * `supervisor`: The [ActorCell] which is to become the supervisor (parent) of this actor
    ///
    /// Returns a [Ok((ActorRef, JoinHandle<Result<JoinHandle<()>, SpawnErr>>))] upon successful creation of the
    /// message queues, so you can begin sending messages. However the associated [JoinHandle] contains the inner
    /// information around if the actor successfully started or not in it's `pre_start` routine. Returns [Err(SpawnErr)] if
    /// the actor name is already allocated
    #[allow(clippy::type_complexity)]
    pub fn spawn_linked_instant(
        name: Option<ActorName>,
        handler: TActor,
        startup_args: TActor::Arguments,
        supervisor: ActorCell,
    ) -> Result<
        (
            ActorRef<TActor>,
            JoinHandle<Result<JoinHandle<()>, SpawnErr>>,
        ),
        SpawnErr,
    > {
        let (actor, ports) = Self::new(name, handler)?;
        let actor_ref = actor.actor_ref.clone();
        let join_op = crate::concurrency::spawn(async move {
            let (_, handle) = actor.start(ports, startup_args, Some(supervisor)).await?;
            Ok(handle)
        });
        Ok((actor_ref, join_op))
    }

    /// Spawn a REMOTE actor with a supervisor, automatically starting the actor. Only for use
    /// by `ractor_cluster::node::NodeSession`
    ///
    /// * `name`: A name to give the actor. Useful for global referencing or debug printing
    /// * `handler`: The [Actor] defining the logic for this actor
    /// * `startup_args`: Arguments passed to the `pre_start` call of the [Actor] to facilitate startup and
    /// initial state creation
    /// * `supervisor`: The [ActorCell] which is to become the supervisor (parent) of this actor
    ///
    /// Returns a [Ok((ActorRef, JoinHandle<()>))] upon successful start, denoting the actor reference
    /// along with the join handle which will complete when the actor terminates. Returns [Err(SpawnErr)] if
    /// the actor failed to start
    #[cfg(feature = "cluster")]
    pub async fn spawn_linked_remote(
        name: Option<ActorName>,
        handler: TActor,
        id: ActorId,
        startup_args: TActor::Arguments,
        supervisor: ActorCell,
    ) -> Result<(ActorRef<TActor>, JoinHandle<()>), SpawnErr> {
        if id.is_local() {
            Err(SpawnErr::StartupPanic(From::from(
                "Cannot spawn a remote actor when the identifier is not remote!",
            )))
        } else {
            let (actor_cell, ports) = actor_cell::ActorCell::new_remote::<TActor>(name, id)?;

            let (actor, ports) = (
                Self {
                    actor_ref: actor_cell.into(),
                    handler,
                },
                ports,
            );
            actor.start(ports, startup_args, Some(supervisor)).await
        }
    }

    /// Create a new actor with some handler implementation and initial state
    ///
    /// * `name`: A name to give the actor. Useful for global referencing or debug printing
    /// * `handler` The [Actor] defining the logic for this actor
    ///
    /// Returns A tuple [(Actor, ActorPortSet)] to be passed to the `start` function of [Actor]
    fn new(name: Option<ActorName>, handler: TActor) -> Result<(Self, ActorPortSet), SpawnErr> {
        let (actor_cell, ports) = actor_cell::ActorCell::new::<TActor>(name)?;
        Ok((
            Self {
                actor_ref: actor_cell.into(),
                handler,
            },
            ports,
        ))
    }

    /// Start the actor immediately, optionally linking to a parent actor (supervision tree)
    ///
    /// NOTE: This returned [crate::concurrency::JoinHandle] is guaranteed to not panic (unless the runtime is shutting down perhaps).
    /// An inner join handle is capturing panic results from any part of the inner tasks, so therefore
    /// we can safely ignore it, or wait on it to block on the actor's progress
    ///
    /// * `ports` - The [ActorPortSet] for this actor
    /// * `supervisor` - The optional [ActorCell] representing the supervisor of this actor
    ///
    /// Returns a [Ok((ActorRef, JoinHandle<()>))] upon successful start, denoting the actor reference
    /// along with the join handle which will complete when the actor terminates. Returns [Err(SpawnErr)] if
    /// the actor failed to start
    async fn start(
        self,
        ports: ActorPortSet,
        startup_args: TActor::Arguments,
        supervisor: Option<ActorCell>,
    ) -> Result<(ActorRef<TActor>, JoinHandle<()>), SpawnErr> {
        // cannot start an actor more than once
        if self.actor_ref.get_status() != ActorStatus::Unstarted {
            return Err(SpawnErr::ActorAlreadyStarted);
        }

        self.actor_ref.set_status(ActorStatus::Starting);

        // Perform the pre-start routine, crashing immediately if we fail to start
        let mut state = Self::do_pre_start(self.actor_ref.clone(), &self.handler, startup_args)
            .await?
            .map_err(SpawnErr::StartupPanic)?;

        // setup supervision
        if let Some(sup) = &supervisor {
            self.actor_ref.link(sup.clone());
        }

        // Generate the ActorRef which will be returned
        let myself_ret = self.actor_ref.clone();

        // run the processing loop, backgrounding the work
        let handle = crate::concurrency::spawn(async move {
            let myself = self.actor_ref.clone();
            let evt = match Self::processing_loop(ports, &mut state, &self.handler, self.actor_ref)
                .await
            {
                Ok(exit_reason) => SupervisionEvent::ActorTerminated(
                    myself.get_cell(),
                    Some(BoxedState::new(state)),
                    exit_reason,
                ),
                Err(actor_err) => match actor_err {
                    ActorErr::Cancelled => SupervisionEvent::ActorTerminated(
                        myself.get_cell(),
                        None,
                        Some("killed".to_string()),
                    ),
                    ActorErr::Panic(msg) => SupervisionEvent::ActorPanicked(myself.get_cell(), msg),
                },
            };

            // terminate children
            myself.terminate();

            // notify supervisors of the actor's death
            myself.notify_supervisor(evt);

            // set status to stopped
            myself.set_status(ActorStatus::Stopped);

            // unlink superisors
            if let Some(sup) = supervisor {
                myself.unlink(sup);
            }
        });

        Ok((myself_ret, handle))
    }

    async fn processing_loop(
        mut ports: ActorPortSet,
        state: &mut TActor::State,
        handler: &TActor,
        myself: ActorRef<TActor>,
    ) -> Result<Option<String>, ActorErr> {
        // perform the post-start, with supervision enabled
        Self::do_post_start(myself.clone(), handler, state)
            .await?
            .map_err(ActorErr::Panic)?;

        myself.set_status(ActorStatus::Running);
        myself.notify_supervisor(SupervisionEvent::ActorStarted(myself.get_cell()));

        let myself_clone = myself.clone();

        let future = async move {
            // the message processing loop. If we get an exit flag, try and capture the exit reason if there
            // is one
            loop {
                let (should_exit, maybe_exit_reason) =
                    Self::process_message(myself.clone(), state, handler, &mut ports)
                        .await
                        .map_err(ActorErr::Panic)?;
                // processing loop exit
                if should_exit {
                    return Ok((state, maybe_exit_reason));
                }
            }
        };

        // capture any panicks in this future and convert to an ActorErr
        let loop_done = futures::FutureExt::catch_unwind(AssertUnwindSafe(future))
            .map_err(|err| ActorErr::Panic(get_panic_string(err)))
            .await;

        // set status to stopping
        myself_clone.set_status(ActorStatus::Stopping);

        let (exit_state, exit_reason) = loop_done??;

        // if we didn't exit in error mode, call `post_stop`
        Self::do_post_stop(myself_clone, handler, exit_state)
            .await?
            .map_err(ActorErr::Panic)?;

        Ok(exit_reason)
    }

    /// Process a message, returning the "new" state (if changed)
    /// along with optionally whether we were signaled mid-processing or not
    ///
    /// * `myself` - The current [ActorRef]
    /// * `state` - The current [Actor::State] object
    /// * `handler` - Pointer to the [Actor] definition
    /// * `ports` - The mutable [ActorPortSet] which are the message ports for this actor
    ///
    /// Returns a tuple of the next [Actor::State] and a flag to denote if the processing
    /// loop is done
    async fn process_message(
        myself: ActorRef<TActor>,
        state: &mut TActor::State,
        handler: &TActor,
        ports: &mut ActorPortSet,
    ) -> Result<(bool, Option<String>), ActorProcessingErr> {
        match ports.listen_in_priority().await {
            Ok(actor_port_message) => match actor_port_message {
                actor_cell::ActorPortMessage::Signal(signal) => {
                    Ok((true, Self::handle_signal(myself, signal)))
                }
                actor_cell::ActorPortMessage::Stop(stop_message) => {
                    let exit_reason = match stop_message {
                        StopMessage::Stop => None,
                        StopMessage::Reason(reason) => Some(reason),
                    };
                    Ok((true, exit_reason))
                }
                actor_cell::ActorPortMessage::Supervision(supervision) => {
                    let new_state_future = Self::handle_supervision_message(
                        myself.clone(),
                        state,
                        handler,
                        supervision,
                    );
                    let new_state = ports.run_with_signal(new_state_future).await;
                    match new_state {
                        Ok(Ok(())) => Ok((false, None)),
                        Ok(Err(internal_err)) => Err(internal_err),
                        Err(signal) => Ok((true, Self::handle_signal(myself, signal))),
                    }
                }
                actor_cell::ActorPortMessage::Message(msg) => {
                    let new_state_future =
                        Self::handle_message(myself.clone(), state, handler, msg);
                    let new_state = ports.run_with_signal(new_state_future).await;
                    match new_state {
                        Ok(Ok(())) => Ok((false, None)),
                        Ok(Err(internal_err)) => Err(internal_err),
                        Err(signal) => Ok((true, Self::handle_signal(myself, signal))),
                    }
                }
            },
            Err(MessagingErr::ChannelClosed) => {
                // one of the channels is closed, this means
                // the receiver was dropped and in this case
                // we should always die. Therefore we flag
                // to terminate
                Ok((true, None))
            }
            Err(MessagingErr::InvalidActorType) => {
                // not possible. Treat like a channel closed
                Ok((true, None))
            }
        }
    }

    async fn handle_message(
        myself: ActorRef<TActor>,
        state: &mut TActor::State,
        handler: &TActor,
        msg: crate::message::BoxedMessage,
    ) -> Result<(), ActorProcessingErr> {
        // panic in order to kill the actor
        #[cfg(feature = "cluster")]
        {
            // A `RemoteActor` will handle serialized messages, without decoding them, forwarding them
            // to the remote system for decoding + handling by the real implementation. Therefore `RemoteActor`s
            // can be thought of as a "shim" to a real actor on a remote system
            if !myself.get_id().is_local() {
                match msg.serialized_msg {
                    Some(serialized_msg) => {
                        return handler
                            .handle_serialized(myself, serialized_msg, state)
                            .await;
                    }
                    None => {
                        return Err(From::from(
                            "`RemoteActor` failed to read `SerializedMessage` from `BoxedMessage`",
                        ));
                    }
                }
            }
        }

        // An error here will bubble up to terminate the actor
        let typed_msg = TActor::Msg::from_boxed(msg)?;
        handler.handle(myself, typed_msg, state).await
    }

    fn handle_signal(myself: ActorRef<TActor>, signal: Signal) -> Option<String> {
        match &signal {
            Signal::Kill => {
                myself.terminate();
            }
        }
        Some(signal.to_string())
    }

    async fn handle_supervision_message(
        myself: ActorRef<TActor>,
        state: &mut TActor::State,
        handler: &TActor,
        message: SupervisionEvent,
    ) -> Result<(), ActorProcessingErr> {
        handler.handle_supervisor_evt(myself, message, state).await
    }

    async fn do_pre_start(
        myself: ActorRef<TActor>,
        handler: &TActor,
        arguments: TActor::Arguments,
    ) -> Result<Result<TActor::State, ActorProcessingErr>, SpawnErr> {
        let future = handler.pre_start(myself, arguments);
        futures::FutureExt::catch_unwind(AssertUnwindSafe(future))
            .await
            .map_err(|err| SpawnErr::StartupPanic(get_panic_string(err)))
    }

    async fn do_post_start(
        myself: ActorRef<TActor>,
        handler: &TActor,
        state: &mut TActor::State,
    ) -> Result<Result<(), ActorProcessingErr>, ActorErr> {
        let future = handler.post_start(myself, state);
        futures::FutureExt::catch_unwind(AssertUnwindSafe(future))
            .await
            .map_err(|err| ActorErr::Panic(get_panic_string(err)))
    }

    async fn do_post_stop(
        myself: ActorRef<TActor>,
        handler: &TActor,
        state: &mut TActor::State,
    ) -> Result<Result<(), ActorProcessingErr>, ActorErr> {
        let future = handler.post_stop(myself, state);
        futures::FutureExt::catch_unwind(AssertUnwindSafe(future))
            .await
            .map_err(|err| ActorErr::Panic(get_panic_string(err)))
    }
}
