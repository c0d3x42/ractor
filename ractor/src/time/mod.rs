// Copyright (c) Sean Lawlor
//
// This source code is licensed under both the MIT license found in the
// LICENSE-MIT file in the root directory of this source tree.

//! Timers for sending messages to actors periodically
//!
//! The methodology of timers in `ractor` are based on [Erlang's `timer` module](https://www.erlang.org/doc/man/timer.html).
//! We aren't supporting all timer functions, as many of them don't make sense but we
//! support the relevant ones for `ractor`. In short
//!
//! 1. Send on a period
//! 2. Send after a delay
//! 3. Stop after a delay
//! 4. Kill after a delay

use crate::concurrency::{Duration, JoinHandle};

use crate::{Actor, ActorCell, MessagingErr, ACTIVE_STATES};

#[cfg(test)]
mod tests;

/// Sends a message to a given actor repeatedly after a specified time
/// using the provided message generation function. The task will exit
/// once the channel is closed (meaning the underlying [crate::Actor]
/// has terminated)
///
/// * `period` - The [Duration] representing the period for the send interval
/// * `actor` - The [ActorCell] representing the [crate::Actor] to communicate with
/// * `msg` - The [Fn] message builder which is called to generate a message for each send
/// operation.
///
/// Returns: The [JoinHandle] which represents the backgrounded work (can be ignored to
/// "fire and forget")
pub fn send_interval<TActor, F>(period: Duration, actor: ActorCell, msg: F) -> JoinHandle<()>
where
    TActor: Actor,
    F: Fn() -> TActor::Msg + Send + 'static,
{
    crate::concurrency::spawn(async move {
        while ACTIVE_STATES.contains(&actor.get_status()) {
            crate::concurrency::sleep(period).await;
            // if we receive an error trying to send, the channel is closed and we should stop trying
            // actor died
            if actor.send_message::<TActor>(msg()).is_err() {
                break;
            }
        }
    })
}

/// Sends a message after a given period to the specified actor. The task terminates
/// once the send has completed
///
/// * `period` - The [Duration] representing the time to delay before sending
/// * `actor` - The [ActorCell] representing the [crate::Actor] to communicate with
/// * `msg` - The [Fn] message builder which is called to generate a message for the send
/// operation
///
/// Returns: The [JoinHandle<Result<(), MessagingErr>>] which represents the backgrounded work.
/// Awaiting the handle will yield the result of the send operation. Can be safely ignored to
/// "fire and forget"
pub fn send_after<TActor, F>(
    period: Duration,
    actor: ActorCell,
    msg: F,
) -> JoinHandle<Result<(), MessagingErr>>
where
    TActor: Actor,
    F: Fn() -> TActor::Msg + Send + 'static,
{
    crate::concurrency::spawn(async move {
        crate::concurrency::sleep(period).await;
        actor.send_message::<TActor>(msg())
    })
}

/// Sends the stop signal to the actor after a specified duration, attaching a reason
/// of "Exit after {}ms" by default
///
/// * `period` - The [Duration] representing the time to delay before sending
/// * `actor` - The [ActorCell] representing the [crate::Actor] to exit after the duration
///
/// Returns: The [JoinHandle] which denotes the backgrounded operation. To cancel the
/// exit operation, you can abort the handle
pub fn exit_after(period: Duration, actor: ActorCell) -> JoinHandle<()> {
    crate::concurrency::spawn(async move {
        crate::concurrency::sleep(period).await;
        actor.stop(Some(format!("Exit after {}ms", period.as_millis())))
    })
}

/// Sends the KILL signal to the actor after a specified duration
///
/// * `period` - The [Duration] representing the time to delay before sending
/// * `actor` - The [ActorCell] representing the [crate::Actor] to kill after the duration
///
/// Returns: The [JoinHandle] which denotes the backgrounded operation. To cancel the
/// kill operation, you can abort the handle
pub fn kill_after(period: Duration, actor: ActorCell) -> JoinHandle<()> {
    crate::concurrency::spawn(async move {
        crate::concurrency::sleep(period).await;
        actor.kill()
    })
}

/// Add the timing functionality on top of the [crate::ActorRef]
impl<TActor> crate::ActorRef<TActor>
where
    TActor: Actor,
{
    /// Alias of [send_interval]
    pub fn send_interval<F>(&self, period: Duration, msg: F) -> JoinHandle<()>
    where
        TActor: Actor,
        F: Fn() -> TActor::Msg + Send + 'static,
    {
        send_interval::<TActor, F>(period, self.get_cell(), msg)
    }

    /// Alias of [send_after]
    pub fn send_after<F>(&self, period: Duration, msg: F) -> JoinHandle<Result<(), MessagingErr>>
    where
        F: Fn() -> TActor::Msg + Send + 'static,
    {
        send_after::<TActor, F>(period, self.get_cell(), msg)
    }

    /// Alias of [exit_after]
    pub fn exit_after(&self, period: Duration) -> JoinHandle<()> {
        exit_after(period, self.get_cell())
    }

    /// Alias of [kill_after]
    pub fn kill_after(&self, period: Duration) -> JoinHandle<()> {
        kill_after(period, self.get_cell())
    }
}
