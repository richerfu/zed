use ohos_hilog_binding::hilog_warn;

use std::{
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

use crate::{
    PlatformDispatcher, Priority, PriorityQueueSender, RealtimePriority, RunnableVariant, TaskLabel, TaskTiming,
    ThreadTaskTimings,
};

pub(crate) struct OhosDispatcher {
    main_thread_id: thread::ThreadId,
    main_sender: PriorityQueueSender<RunnableVariant>,
}

impl OhosDispatcher {
    pub(crate) fn new(main_sender: PriorityQueueSender<RunnableVariant>) -> Self {
        Self {
            main_thread_id: thread::current().id(),
            main_sender,
        }
    }
    
    pub(crate) fn execute_runnable(runnable: RunnableVariant) {
        match runnable {
            RunnableVariant::Meta(runnable) => {
                runnable.run();
            }
            RunnableVariant::Compat(runnable) => {
                runnable.run();
            }
        }
    }
}

impl PlatformDispatcher for OhosDispatcher {
    fn get_all_timings(&self) -> Vec<ThreadTaskTimings> {
        Vec::new()
    }

    fn get_current_thread_timings(&self) -> Vec<TaskTiming> {
        Vec::new()
    }

    fn is_main_thread(&self) -> bool {
        thread::current().id() == self.main_thread_id
    }

    fn dispatch(&self, runnable: RunnableVariant, _label: Option<TaskLabel>, _priority: Priority) {
        // On OHOS, we dispatch directly on the current thread
        // In a real implementation, we might want to use OpenHarmonyApp's event loop
        match runnable {
            RunnableVariant::Meta(runnable) => {
                runnable.run();
            }
            RunnableVariant::Compat(runnable) => {
                runnable.run();
            }
        }
    }

    fn dispatch_on_main_thread(
        &self,
        runnable: RunnableVariant,
        priority: Priority,
    ) {
        match self.main_sender.send(priority, runnable) {
            Ok(_) => {
                // Task has been queued, it will be processed in the run_loop callback
            }
            Err(runnable) => {
                // NOTE: Runnable may wrap a Future that is !Send.
                //
                // This is usually safe because we only poll it on the main thread.
                // However if the send fails, we know that:
                // 1. main_receiver has been dropped (which implies the app is shutting down)
                // 2. we are on a background thread.
                // It is not safe to drop something !Send on the wrong thread, and
                // the app will exit soon anyway, so we must forget the runnable.
                std::mem::forget(runnable);
            }
        }
    }

    fn dispatch_after(&self, duration: Duration, runnable: RunnableVariant) {
        // TODO: Implement timer support using OpenHarmonyApp's event loop
        hilog_warn!("dispatch_after not fully implemented on OHOS");
        thread::sleep(duration);
        match runnable {
            RunnableVariant::Meta(runnable) => {
                runnable.run();
            }
            RunnableVariant::Compat(runnable) => {
                runnable.run();
            }
        }
    }

    fn spawn_realtime(&self, _priority: RealtimePriority, f: Box<dyn FnOnce() + Send>) {
        thread::spawn(f);
    }

    fn now(&self) -> Instant {
        Instant::now()
    }
}

