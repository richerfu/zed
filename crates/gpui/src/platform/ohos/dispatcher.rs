use std::{
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

use crate::{
    PlatformDispatcher, Priority, RealtimePriority, RunnableVariant, TaskLabel, TaskTiming,
    ThreadTaskTimings,
};

pub(crate) struct OhosDispatcher {
    main_thread_id: thread::ThreadId,
}

impl OhosDispatcher {
    pub(crate) fn new() -> Self {
        Self {
            main_thread_id: thread::current().id(),
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
        _priority: Priority,
    ) {
        if self.is_main_thread() {
            match runnable {
                RunnableVariant::Meta(runnable) => {
                    runnable.run();
                }
                RunnableVariant::Compat(runnable) => {
                    runnable.run();
                }
            }
        } else {
            // TODO: Post to main thread via OpenHarmonyApp's event loop
            log::warn!("dispatch_on_main_thread called from non-main thread");
        }
    }

    fn dispatch_after(&self, duration: Duration, runnable: RunnableVariant) {
        // TODO: Implement timer support using OpenHarmonyApp's event loop
        log::warn!("dispatch_after not fully implemented on OHOS");
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

