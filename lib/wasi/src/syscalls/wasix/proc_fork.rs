use super::*;
use crate::{
    capture_snapshot,
    os::task::OwnedTaskStatus,
    runtime::task_manager::{TaskWasm, TaskWasmRunProperties},
    syscalls::*,
    WasiThreadHandle,
};
use wasmer::Memory;

/// ### `proc_fork()`
/// Forks the current process into a new subprocess. If the function
/// returns a zero then its the new subprocess. If it returns a positive
/// number then its the current process and the $pid represents the child.
#[instrument(level = "debug", skip_all, fields(pid = ctx.data().process.pid().raw()), ret, err)]
pub fn proc_fork<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    mut copy_memory: Bool,
    pid_ptr: WasmPtr<Pid, M>,
) -> Result<Errno, WasiError> {
    wasi_try_ok!(WasiEnv::process_signals_and_exit(&mut ctx)?);

    // If we were just restored then we need to return the value instead
    if handle_rewind::<M>(&mut ctx) {
        let view = ctx.data().memory_view(&ctx);
        let ret_pid = pid_ptr.read(&view).unwrap_or(u32::MAX);
        if ret_pid == 0 {
            trace!("handle_rewind - i am child");
        } else {
            trace!("handle_rewind - i am parent (child={})", ret_pid);
        }
        return Ok(Errno::Success);
    }
    trace!(%copy_memory, "capturing");

    // Fork the environment which will copy all the open file handlers
    // and associate a new context but otherwise shares things like the
    // file system interface. The handle to the forked process is stored
    // in the parent process context
    let (mut child_env, mut child_handle) = match ctx.data().fork() {
        Ok(p) => p,
        Err(err) => {
            debug!("could not fork process: {err}");
            // TODO: evaluate the appropriate error code, document it in the spec.
            return Ok(Errno::Perm);
        }
    };
    let child_pid = child_env.process.pid();
    let child_finished = child_env.process.finished.clone();

    // We write a zero to the PID before we capture the stack
    // so that this is what will be returned to the child
    {
        let mut inner = ctx.data().process.inner.write().unwrap();
        inner.children.push(child_env.process.clone());
    }
    let env = ctx.data();
    let memory = env.memory_view(&ctx);

    // Setup some properties in the child environment
    wasi_try_mem_ok!(pid_ptr.write(&memory, 0));
    let pid = child_env.pid();
    let tid = child_env.tid();

    // Pass some offsets to the unwind function
    let pid_offset = pid_ptr.offset();

    // If we are not copying the memory then we act like a `vfork`
    // instead which will pretend to be the new process for a period
    // of time until `proc_exec` is called at which point the fork
    // actually occurs
    if copy_memory == Bool::False {
        // Perform the unwind action
        let pid_offset: u64 = pid_offset.into();
        return unwind::<M, _>(ctx, move |mut ctx, mut memory_stack, rewind_stack| {
            // Grab all the globals and serialize them
            let store_data = crate::utils::store::capture_snapshot(&mut ctx.as_store_mut())
                .serialize()
                .unwrap();
            let store_data = Bytes::from(store_data);

            // We first fork the environment and replace the current environment
            // so that the process can continue to prepare for the real fork as
            // if it had actually forked
            child_env.swap_inner(ctx.data_mut());
            std::mem::swap(ctx.data_mut(), &mut child_env);
            ctx.data_mut().vfork.replace(WasiVFork {
                rewind_stack: rewind_stack.clone(),
                memory_stack: memory_stack.clone(),
                store_data: store_data.clone(),
                env: Box::new(child_env),
                handle: child_handle,
                pid_offset,
            });

            // Carry on as if the fork had taken place (which basically means
            // it prevents to be the new process with the old one suspended)
            // Rewind the stack and carry on
            match rewind::<M>(
                ctx,
                memory_stack.freeze(),
                rewind_stack.freeze(),
                store_data,
            ) {
                Errno::Success => OnCalledAction::InvokeAgain,
                err => {
                    warn!("failed - could not rewind the stack - errno={}", err);
                    OnCalledAction::Trap(Box::new(WasiError::Exit(err.into())))
                }
            }
        });
    }

    // Create the thread that will back this forked process
    let state = env.state.clone();
    let bin_factory = env.bin_factory.clone();

    // Perform the unwind action
    let snapshot = capture_snapshot(&mut ctx.as_store_mut());
    unwind::<M, _>(ctx, move |mut ctx, mut memory_stack, rewind_stack| {
        let span = debug_span!(
            "unwind",
            memory_stack_len = memory_stack.len(),
            rewind_stack_len = rewind_stack.len()
        );
        let _span_guard = span.enter();

        // Grab all the globals and serialize them
        let store_data = snapshot.serialize().unwrap();
        let store_data = Bytes::from(store_data);

        // Now we use the environment and memory references
        let runtime = child_env.runtime.clone();
        let tasks = child_env.tasks().clone();
        let child_memory_stack = memory_stack.clone();
        let child_rewind_stack = rewind_stack.clone();

        let module = ctx.data().inner().module_clone();
        let memory = ctx.data().memory_clone();
        let spawn_type = SpawnMemoryType::CopyMemory(memory, ctx.as_store_ref());

        // Spawn a new process with this current execution environment
        let signaler = Box::new(child_env.process.clone());
        {
            let runtime = runtime.clone();
            let tasks = tasks.clone();
            let tasks_outer = tasks.clone();
            let store_data = store_data.clone();

            let run = move |mut props: TaskWasmRunProperties| {
                let ctx = props.ctx;
                let mut store = props.store;

                // Rewind the stack and carry on
                {
                    trace!("rewinding child");
                    let mut ctx = ctx.env.clone().into_mut(&mut store);
                    let (data, mut store) = ctx.data_and_store_mut();
                    match rewind::<M>(
                        ctx,
                        child_memory_stack.freeze(),
                        child_rewind_stack.freeze(),
                        store_data.clone(),
                    ) {
                        Errno::Success => OnCalledAction::InvokeAgain,
                        err => {
                            warn!(
                                "wasm rewind failed - could not rewind the stack - errno={}",
                                err
                            );
                            return;
                        }
                    };
                }

                // Invoke the start function
                run::<M>(ctx, store, child_handle, None);
            };

            tasks_outer
                .task_wasm(
                    TaskWasm::new(Box::new(run), child_env, module, false)
                        .with_snapshot(&snapshot)
                        .with_memory(spawn_type),
                )
                .map_err(|err| {
                    warn!(
                        "failed to fork as the process could not be spawned - {}",
                        err
                    );
                    err
                })
                .ok()
        };

        // If the return value offset is within the memory stack then we need
        // to update it here rather than in the real memory
        let env = ctx.data();
        let val_bytes = child_pid.raw().to_ne_bytes();
        let pid_offset: u64 = pid_offset.into();
        if pid_offset >= env.layout.stack_lower
            && (pid_offset + val_bytes.len() as u64) <= env.layout.stack_upper
        {
            // Make sure its within the "active" part of the memory stack
            let offset = env.layout.stack_upper - pid_offset;
            if offset as usize > memory_stack.len() {
                warn!(
                        "failed - the return value (pid) is outside of the active part of the memory stack ({} vs {})",
                        offset,
                        memory_stack.len()
                    );
                return OnCalledAction::Trap(Box::new(WasiError::Exit(Errno::Memviolation.into())));
            }

            // Update the memory stack with the new PID
            let pstart = memory_stack.len() - offset as usize;
            let pend = pstart + val_bytes.len();
            let pbytes = &mut memory_stack[pstart..pend];
            pbytes.clone_from_slice(&val_bytes);
        } else {
            warn!(
                    "failed - the return value (pid) is not being returned on the stack - which is not supported"
                );
            return OnCalledAction::Trap(Box::new(WasiError::Exit(Errno::Memviolation.into())));
        }

        // Rewind the stack and carry on
        match rewind::<M>(
            ctx,
            memory_stack.freeze(),
            rewind_stack.freeze(),
            store_data,
        ) {
            Errno::Success => OnCalledAction::InvokeAgain,
            err => {
                warn!("failed - could not rewind the stack - errno={}", err);
                OnCalledAction::Trap(Box::new(WasiError::Exit(err.into())))
            }
        }
    })
}

fn run<M: MemorySize>(
    ctx: WasiFunctionEnv,
    mut store: Store,
    child_handle: WasiThreadHandle,
    rewind_state: Option<(RewindState, Result<(), Errno>)>,
) -> ExitCode {
    let env = ctx.data(&store);
    let tasks = env.tasks().clone();
    let pid = env.pid();
    let tid = env.tid();

    // If we need to rewind then do so
    if let Some((mut rewind_state, trigger_res)) = rewind_state {
        if let Err(exit_code) = rewind_state.rewinding_finish::<M>(&ctx, &mut store, trigger_res) {
            return exit_code;
        }
        let res = rewind::<M>(
            ctx.env.clone().into_mut(&mut store),
            rewind_state.memory_stack,
            rewind_state.rewind_stack,
            rewind_state.store_data,
        );
        if res != Errno::Success {
            return res.into();
        }
    }

    let mut ret: ExitCode = Errno::Success.into();
    let err = if ctx.data(&store).thread.is_main() {
        trace!(%pid, %tid, "re-invoking main");
        let start = ctx.data(&store).inner().start.clone().unwrap();
        start.call(&mut store)
    } else {
        trace!(%pid, %tid, "re-invoking thread_spawn");
        let start = ctx.data(&store).inner().thread_spawn.clone().unwrap();
        start.call(&mut store, 0, 0)
    };
    if let Err(err) = err {
        match err.downcast::<WasiError>() {
            Ok(WasiError::Exit(exit_code)) => {
                ret = exit_code;
            }
            Ok(WasiError::DeepSleep(deep)) => {
                trace!(%pid, %tid, "entered a deep sleep");

                // Create the respawn function
                let respawn = {
                    let tasks = tasks.clone();
                    let rewind_state = deep.rewind;
                    move |ctx, store, trigger_res| {
                        run::<M>(ctx, store, child_handle, Some((rewind_state, trigger_res)));
                    }
                };

                /// Spawns the WASM process after a trigger
                tasks.resume_wasm_after_poller(Box::new(respawn), ctx, store, deep.trigger);
                return Errno::Success.into();
            }
            _ => {}
        }
    }
    trace!(%pid, %tid, "child exited (code = {})", ret);

    // Clean up the environment and return the result
    ctx.cleanup((&mut store), Some(ret));

    // We drop the handle at the last moment which will close the thread
    drop(child_handle);
    ret
}
