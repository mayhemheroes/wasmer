use std::f32::consts::E;

use wasmer_wasix_types::wasi::SubscriptionClock;

use super::*;
use crate::{
    fs::{InodeValFilePollGuard, InodeValFilePollGuardJoin},
    state::PollEventSet,
    syscalls::*,
    WasiInodes,
};

/// ### `poll_oneoff()`
/// Concurrently poll for a set of events
/// Inputs:
/// - `const __wasi_subscription_t *in`
///     The events to subscribe to
/// - `__wasi_event_t *out`
///     The events that have occured
/// - `u32 nsubscriptions`
///     The number of subscriptions and the number of events
/// Output:
/// - `u32 nevents`
///     The number of events seen
#[instrument(level = "trace", skip_all, fields(timeout_ms = field::Empty, fd_guards = field::Empty, seen = field::Empty), ret, err)]
pub fn poll_oneoff<M: MemorySize + 'static>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    in_: WasmPtr<Subscription, M>,
    out_: WasmPtr<Event, M>,
    nsubscriptions: M::Offset,
    nevents: WasmPtr<M::Offset, M>,
) -> Result<Errno, WasiError> {
    wasi_try_ok!(WasiEnv::process_signals_and_exit(&mut ctx)?);

    ctx.data_mut().poll_seed += 1;
    let mut env = ctx.data();
    let mut memory = env.memory_view(&ctx);

    let subscription_array = wasi_try_mem_ok!(in_.slice(&memory, nsubscriptions));
    let mut subscriptions = Vec::with_capacity(subscription_array.len() as usize);
    for n in 0..subscription_array.len() {
        let n = (n + env.poll_seed) % subscription_array.len();
        let sub = subscription_array.index(n);
        let s = wasi_try_mem_ok!(sub.read());
        subscriptions.push((None, PollEventSet::default(), s));
    }

    // We clear the number of events
    wasi_try_mem_ok!(nevents.write(&memory, M::ZERO));

    // Function to invoke once the poll is finished
    let process_events = move |memory: &'_ Memory, store: &'_ dyn AsStoreRef, triggered_events| {
        // Process all the events that were triggered
        let mut view = memory.view(store);
        let mut events_seen: u32 = 0;
        let event_array = wasi_try_mem!(out_.slice(&view, nsubscriptions));
        for event in triggered_events {
            wasi_try_mem!(event_array.index(events_seen as u64).write(event));
            events_seen += 1;
        }
        let events_seen: M::Offset = wasi_try!(events_seen.try_into().map_err(|_| Errno::Overflow));
        let out_ptr = nevents.deref(&view);
        wasi_try_mem!(out_ptr.write(events_seen));
        Errno::Success
    };

    // Poll and receive all the events that triggered
    poll_oneoff_internal::<M, _>(ctx, subscriptions, process_events)
}

struct PollBatch {
    pid: WasiProcessId,
    tid: WasiThreadId,
    evts: Vec<Event>,
    joins: Vec<InodeValFilePollGuardJoin>,
}
impl PollBatch {
    fn new(pid: WasiProcessId, tid: WasiThreadId, fds: Vec<InodeValFilePollGuard>) -> Self {
        Self {
            pid,
            tid,
            evts: Vec::new(),
            joins: fds
                .into_iter()
                .map(InodeValFilePollGuardJoin::new)
                .collect(),
        }
    }
}
impl Future for PollBatch {
    type Output = Result<Vec<Event>, Errno>;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let pid = self.pid;
        let tid = self.tid;
        let mut done = false;

        let mut evts = Vec::new();
        for mut join in self.joins.iter_mut() {
            let fd = join.fd();
            let peb = join.peb();
            let mut guard = Pin::new(join);
            match guard.poll(cx) {
                Poll::Pending => {}
                Poll::Ready(e) => {
                    for evt in e {
                        tracing::trace!(
                            fd,
                            userdata = evt.userdata,
                            ty = evt.type_ as u8,
                            peb,
                            "triggered"
                        );
                        evts.push(evt);
                    }
                }
            }
        }

        if !evts.is_empty() {
            return Poll::Ready(Ok(evts));
        }

        Poll::Pending
    }
}

/// ### `poll_oneoff()`
/// Concurrently poll for a set of events
/// Inputs:
/// - `const __wasi_subscription_t *in`
///     The events to subscribe to
/// - `__wasi_event_t *out`
///     The events that have occured
/// - `u32 nsubscriptions`
///     The number of subscriptions and the number of events
/// Output:
/// - `u32 nevents`
///     The number of events seen
pub(crate) fn poll_oneoff_internal<M: MemorySize, After>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    mut subs: Vec<(Option<WasiFd>, PollEventSet, Subscription)>,
    process_events: After,
) -> Result<Errno, WasiError>
where
    After: FnOnce(&'_ Memory, &'_ dyn AsStoreRef, Vec<Event>) -> Errno + Send + Sync + 'static,
{
    wasi_try_ok!(WasiEnv::process_signals_and_exit(&mut ctx)?);
    if handle_rewind::<M>(&mut ctx) {
        return Ok(Errno::Success);
    }

    let pid = ctx.data().pid();
    let tid = ctx.data().tid();

    // Determine if we are in silent polling mode
    let mut env = ctx.data();
    let state = ctx.data().state.deref();

    // These are used when we capture what clocks (timeouts) are being
    // subscribed too
    let clock_cnt = subs
        .iter()
        .filter(|a| a.2.type_ == Eventtype::Clock)
        .count();
    let mut clock_subs: Vec<(SubscriptionClock, u64)> = Vec::with_capacity(subs.len());
    let mut time_to_sleep = Duration::MAX;

    // First we extract all the subscriptions into an array so that they
    // can be processed
    let mut memory = env.memory_view(&ctx);
    for (fd, peb, s) in subs.iter_mut() {
        let fd = match s.type_ {
            Eventtype::FdRead => {
                let file_descriptor = unsafe { s.data.fd_readwrite.file_descriptor };
                match file_descriptor {
                    __WASI_STDIN_FILENO | __WASI_STDOUT_FILENO | __WASI_STDERR_FILENO => (),
                    fd => {
                        let fd_entry = match state.fs.get_fd(fd) {
                            Ok(a) => a,
                            Err(err) => return Ok(err),
                        };
                        if !fd_entry.rights.contains(Rights::POLL_FD_READWRITE) {
                            return Ok(Errno::Access);
                        }
                    }
                }
                *fd = Some(file_descriptor);
                *peb |= (PollEvent::PollIn as PollEventSet);
                file_descriptor
            }
            Eventtype::FdWrite => {
                let file_descriptor = unsafe { s.data.fd_readwrite.file_descriptor };
                match file_descriptor {
                    __WASI_STDIN_FILENO | __WASI_STDOUT_FILENO | __WASI_STDERR_FILENO => (),
                    fd => {
                        let fd_entry = match state.fs.get_fd(fd) {
                            Ok(a) => a,
                            Err(err) => return Ok(err),
                        };
                        if !fd_entry.rights.contains(Rights::POLL_FD_READWRITE) {
                            return Ok(Errno::Access);
                        }
                    }
                }
                *fd = Some(file_descriptor);
                *peb |= (PollEvent::PollOut as PollEventSet);
                file_descriptor
            }
            Eventtype::Clock => {
                let clock_info = unsafe { s.data.clock };
                if clock_info.clock_id == Clockid::Realtime
                    || clock_info.clock_id == Clockid::Monotonic
                {
                    // Ignore duplicates
                    if clock_subs
                        .iter()
                        .any(|c| c.0.clock_id == clock_info.clock_id && c.1 == s.userdata)
                    {
                        continue;
                    }

                    // If the timeout duration is zero then this is an immediate check rather than
                    // a sleep itself
                    if clock_info.timeout == 0 {
                        time_to_sleep = Duration::MAX;
                    } else if clock_info.timeout == 1 {
                        time_to_sleep = Duration::ZERO;
                    } else {
                        time_to_sleep = Duration::from_nanos(clock_info.timeout);
                        clock_subs.push((clock_info, s.userdata));
                    }
                    continue;
                } else {
                    error!("polling not implemented for these clocks yet");
                    return Ok(Errno::Inval);
                }
            }
        };
    }

    let mut events_seen: u32 = 0;

    let batch = {
        // Build the batch of things we are going to poll
        let state = ctx.data().state.clone();
        let tasks = ctx.data().tasks().clone();
        let mut guards = {
            // We start by building a list of files we are going to poll
            // and open a read lock on them all
            let mut fd_guards = Vec::with_capacity(subs.len());

            #[allow(clippy::significant_drop_in_scrutinee)]
            for (fd, peb, s) in subs {
                if let Some(fd) = fd {
                    let wasi_file_ref = match fd {
                        __WASI_STDERR_FILENO => {
                            wasi_try_ok!(WasiInodes::stderr(&state.fs.fd_map)
                                .map(|g| g.into_poll_guard(fd, peb, s))
                                .map_err(fs_error_into_wasi_err))
                        }
                        __WASI_STDOUT_FILENO => {
                            wasi_try_ok!(WasiInodes::stdout(&state.fs.fd_map)
                                .map(|g| g.into_poll_guard(fd, peb, s))
                                .map_err(fs_error_into_wasi_err))
                        }
                        _ => {
                            let fd_entry = wasi_try_ok!(state.fs.get_fd(fd));
                            if !fd_entry.rights.contains(Rights::POLL_FD_READWRITE) {
                                return Ok(Errno::Access);
                            }
                            let inode = fd_entry.inode;

                            {
                                let guard = inode.read();
                                if let Some(guard) =
                                    crate::fs::InodeValFilePollGuard::new(fd, peb, s, guard.deref())
                                {
                                    guard
                                } else {
                                    return Ok(Errno::Badf);
                                }
                            }
                        }
                    };
                    fd_guards.push(wasi_file_ref);
                }
            }

            if fd_guards.len() > 10 {
                let small_list: Vec<_> = fd_guards.iter().take(10).collect();
                tracing::Span::current().record("fd_guards", format!("{:?}...", small_list));
            } else {
                tracing::Span::current().record("fd_guards", format!("{:?}", fd_guards));
            }

            fd_guards
        };

        // Block polling the file descriptors
        PollBatch::new(pid, tid, guards)
    };

    // If the time is infinite then we omit the time_to_sleep parameter
    let asyncify_time = match time_to_sleep {
        Duration::ZERO => {
            Span::current().record("timeout_ns", "nonblocking");
            Some(Duration::ZERO)
        }
        Duration::MAX => {
            Span::current().record("timeout_ns", "infinite");
            None
        }
        time => {
            Span::current().record("timeout_ns", time.as_millis());
            Some(time)
        }
    };

    // We use asyncify with a deep sleep to wait on new IO events
    let res = __asyncify_with_deep_sleep_ext::<M, _, _, _>(
        ctx,
        asyncify_time,
        Duration::from_millis(50),
        batch,
        move |memory, store, res| {
            let events = res.unwrap_or_else(Err);
            // Process the result
            match events {
                Ok(evts) => {
                    // If its a timeout then return an event for it
                    Span::current().record("seen", evts.len());

                    // Process the events
                    process_events(memory, store, evts);
                }
                Err(Errno::Timedout) => {
                    // The timeout has triggerred so lets add that event
                    if clock_subs.is_empty() {
                        tracing::warn!("triggered_timeout (without any clock subscriptions)",);
                    }
                    let mut evts = Vec::new();
                    for (clock_info, userdata) in clock_subs {
                        let evt = Event {
                            userdata,
                            error: Errno::Success,
                            type_: Eventtype::Clock,
                            u: EventUnion { clock: 0 },
                        };
                        Span::current().record(
                            "seen",
                            &format!(
                                "clock(id={},userdata={})",
                                clock_info.clock_id as u32, evt.userdata
                            ),
                        );
                        evts.push(evt);
                    }
                    process_events(memory, store, evts);
                }
                // If nonblocking the Errno::Again needs to be turned into an empty list
                Err(Errno::Again) => {
                    process_events(memory, store, Default::default());
                }
                // Otherwise process the error
                Err(err) => {
                    tracing::warn!("failed to poll during deep sleep - {}", err);
                }
            }
            Ok(())
        },
    )?;
    Ok(Errno::Success)
}
