use log::warn;

use std::{
    cmp::Ordering,
    collections::BinaryHeap,
    sync::{Arc, Condvar, Mutex},
    thread,
    time::{Duration, Instant},
};

use crate::{
    PlatformDispatcher, Priority, PriorityQueueSender, RealtimePriority, RunnableVariant,
    TaskLabel, TaskTiming, ThreadTaskTimings,
};
use openharmony_ability::OpenHarmonyWaker;

struct TimerAfter {
    when: Instant,
    runnable: RunnableVariant,
}

impl Ord for TimerAfter {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reverse for min-heap behavior
        other.when.cmp(&self.when)
    }
}

impl PartialOrd for TimerAfter {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for TimerAfter {
    fn eq(&self, other: &Self) -> bool {
        self.when.eq(&other.when)
    }
}

impl Eq for TimerAfter {}

pub(crate) struct OhosDispatcher {
    main_thread_id: thread::ThreadId,
    main_sender: PriorityQueueSender<RunnableVariant>,
    timer_queue: Arc<(Mutex<BinaryHeap<TimerAfter>>, Condvar)>,
    waker: Arc<Mutex<Option<OpenHarmonyWaker>>>,
    _timer_thread: thread::JoinHandle<()>,
}

impl OhosDispatcher {
    pub(crate) fn new(main_sender: PriorityQueueSender<RunnableVariant>) -> Self {
        let timer_queue: Arc<(Mutex<BinaryHeap<TimerAfter>>, Condvar)> =
            Arc::new((Mutex::new(BinaryHeap::new()), Condvar::new()));
        let waker: Arc<Mutex<Option<OpenHarmonyWaker>>> = Arc::new(Mutex::new(None));
        let timer_queue_thread = timer_queue.clone();
        let waker_thread = waker.clone();
        let timer_thread = thread::Builder::new()
            .name("OhosTimer".to_owned())
            .spawn(move || {
                loop {
                    let (lock, cvar) = &*timer_queue_thread;
                    let mut heap = lock.lock().unwrap();

                    loop {
                        if let Some(next) = heap.peek() {
                            let now = Instant::now();
                            if next.when <= now {
                                break;
                            }
                            let timeout = next.when.saturating_duration_since(now);
                            let (new_heap, _) = cvar.wait_timeout(heap, timeout).unwrap();
                            heap = new_heap;
                        } else {
                            heap = cvar.wait(heap).unwrap();
                        }
                    }

                    drop(heap);
                    if let Some(waker) = waker_thread.lock().unwrap().as_ref() {
                        waker.wake();
                    }
                }
            })
            .expect("Failed to start OHOS timer thread");

        Self {
            main_thread_id: thread::current().id(),
            main_sender,
            timer_queue,
            waker,
            _timer_thread: timer_thread,
        }
    }

    pub(crate) fn set_waker(&self, waker: OpenHarmonyWaker) {
        *self.waker.lock().unwrap() = Some(waker);
    }

    pub(crate) fn run_due_timers(&self) {
        let mut due = Vec::new();
        let now = Instant::now();
        {
            let (lock, _) = &*self.timer_queue;
            let mut heap = lock.lock().unwrap();
            while let Some(next) = heap.peek() {
                if next.when > now {
                    break;
                }
                due.push(heap.pop().expect("timer entry exists").runnable);
            }
        }

        for runnable in due {
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
        // On OHOS, run background tasks off the main thread to avoid UI stalls.
        std::thread::spawn(move || match runnable {
            RunnableVariant::Meta(runnable) => {
                runnable.run();
            }
            RunnableVariant::Compat(runnable) => {
                runnable.run();
            }
        });
    }

    fn dispatch_on_main_thread(&self, runnable: RunnableVariant, priority: Priority) {
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
        let (lock, cvar) = &*self.timer_queue;
        let mut heap = lock.lock().unwrap();
        heap.push(TimerAfter {
            when: Instant::now() + duration,
            runnable,
        });
        cvar.notify_one();
    }

    fn spawn_realtime(&self, _priority: RealtimePriority, f: Box<dyn FnOnce() + Send>) {
        thread::spawn(f);
    }

    fn now(&self) -> Instant {
        Instant::now()
    }
}
