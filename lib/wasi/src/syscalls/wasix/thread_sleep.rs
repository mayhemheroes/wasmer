use super::*;
use crate::syscalls::*;

/// ### `thread_sleep()`
/// Sends the current thread to sleep for a period of time
///
/// ## Parameters
///
/// * `duration` - Amount of time that the thread should sleep
#[instrument(level = "debug", skip_all, fields(%duration), ret, err)]
pub fn thread_sleep<M: MemorySize + 'static>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    duration: Timestamp,
) -> Result<Errno, WasiError> {
    thread_sleep_internal::<M>(ctx, duration)
}

pub(crate) fn thread_sleep_internal<M: MemorySize + 'static>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    duration: Timestamp,
) -> Result<Errno, WasiError> {
    wasi_try_ok!(WasiEnv::process_signals_and_exit(&mut ctx)?);
    if handle_rewind::<M>(&mut ctx) {
        return Ok(Errno::Success);
    }

    let env = ctx.data();

    #[cfg(feature = "sys-thread")]
    if duration == 0 {
        std::thread::yield_now();
    }

    if duration > 0 {
        let duration = Duration::from_nanos(duration as u64);
        let tasks = env.tasks().clone();

        __asyncify_with_deep_sleep_ext::<M, _, _, _>(
            ctx,
            Some(duration),
            Duration::from_millis(50),
            async move {
                // using an infinite async sleep here means we don't have to write the same event
                // handling loop code for signals and timeouts
                InfiniteSleep::default().await;
                unreachable!(
                    "the timeout or signals will wake up this thread even though it waits forever"
                )
            },
            |_, _, _| Ok(()),
        )?;
    }
    Ok(Errno::Success)
}
