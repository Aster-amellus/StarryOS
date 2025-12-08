use alloc::{borrow::Cow, sync::Arc};
use core::{
    any::Any,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use axerrno::{AxError, AxResult};
use axhal::time::{monotonic_time, wall_time, TimeValue};
use axio::{BufMut, Write};
use axpoll::{Pollable, IoEvents, PollSet};
use axsync::Mutex;
use axtask::future::{block_on, poll_io, timeout_at};
use event_listener::{Event, listener};
use linux_raw_sys::general::{CLOCK_MONOTONIC, CLOCK_REALTIME, itimerspec};

use crate::file::{FileLike, Kstat, SealedBuf, SealedBufMut};

#[allow(dead_code)]
#[derive(Debug, Copy, Clone)]
struct TimerState {
    ticks: u64,
    interval: Duration,
    next_expiration: Option<TimeValue>,
}

#[allow(dead_code)]
pub struct TimerFd {
    clockid: i32,
    state: Mutex<TimerState>,
    non_blocking: AtomicBool,
    poll_read: PollSet,
    update_event: Event,
}

#[allow(dead_code)]
impl TimerFd {
    pub fn new(clockid: i32, _flags: i32) -> AxResult<Arc<Self>> {
        let timer = Arc::new(Self {
            clockid,
            state: Mutex::new(TimerState {
                ticks: 0,
                interval: Duration::ZERO,
                next_expiration: None,
            }),
            non_blocking: AtomicBool::new(false),
            poll_read: PollSet::new(),
            update_event: Event::new(),
        });

        let t = timer.clone();
        axtask::spawn(move || block_on(t.timer_loop()));
        Ok(timer)
    }

    pub fn current_time(&self) -> TimeValue {
        match self.clockid {
            c if c == CLOCK_MONOTONIC as i32 => monotonic_time(),
            c if c == CLOCK_REALTIME as i32 => wall_time(),
            _ => monotonic_time(),
        }
    }

    pub fn set_time(
        &self,
        flags: i32,
        new_value: &itimerspec,
        old_value: Option<&mut itimerspec>,
    ) -> AxResult<()> {
        let mut state = self.state.lock();

        if let Some(old) = old_value {
            if let Some(exp) = state.next_expiration {
                let now = self.current_time();
                let remaining = exp.saturating_sub(now);

                old.it_value.tv_sec = remaining.as_secs() as _;
                old.it_value.tv_nsec = remaining.subsec_nanos() as _;
                old.it_interval.tv_sec = state.interval.as_secs() as _;
                old.it_interval.tv_nsec = state.interval.subsec_nanos() as _;
            } else {
                *old = itimerspec {
                    it_interval: linux_raw_sys::general::timespec { tv_sec: 0, tv_nsec: 0 },
                    it_value: linux_raw_sys::general::timespec { tv_sec: 0, tv_nsec: 0 },
                };
            }
        }

        let interval = Duration::new(
            new_value.it_interval.tv_sec as u64,
            new_value.it_interval.tv_nsec as u32,
        );

        let value = Duration::new(
            new_value.it_value.tv_sec as u64,
            new_value.it_value.tv_nsec as u32,
        );
        state.interval = interval;

        if value.is_zero() {
            state.next_expiration = None;
        } else {
            // Arm timer
            let now = self.current_time();
            let target = if flags & 1 != 0 {
                TimeValue::from_nanos(value.as_nanos() as u64)
            } else {
                // Relative time
                now + value
            };
            state.next_expiration = Some(target);
        }

        self.update_event.notify(usize::MAX);
        Ok(())
    }

    pub fn set_nonblocking(&self, flag: bool) -> AxResult<()> {
        self.non_blocking.store(flag, Ordering::Release);
        Ok(())
    }
    
    pub fn get_time(&self, curr_value: &mut itimerspec) {
        let state = self.state.lock();
        let now = self.current_time();

        let remaining = state
            .next_expiration
            .map(|exp| exp.saturating_sub(now))
            .unwrap_or(Duration::ZERO);

        curr_value.it_value.tv_sec = remaining.as_secs() as _;
        curr_value.it_value.tv_nsec = remaining.subsec_nanos() as _;
        curr_value.it_interval.tv_sec = state.interval.as_secs() as _;
        curr_value.it_interval.tv_nsec = state.interval.subsec_nanos() as _;
    }

    async fn timer_loop(&self) {
        loop {
            let target = {
                let state = self.state.lock();
                state.next_expiration
            };

            if let Some(target_time) = target {
                // Translate the monotonic-based target to the wall-clock deadline used by timeout_at.
                let now_mono = self.current_time();
                let delta = target_time.saturating_sub(now_mono);

                if delta.is_zero() {
                    let mut state = self.state.lock();
                    if state.next_expiration == Some(target_time) {
                        state.ticks += 1;
                        self.poll_read.wake();

                        if !state.interval.is_zero() {
                            state.next_expiration = Some(now_mono + state.interval);
                        } else {
                            state.next_expiration = None;
                        }
                    }
                    continue;
                }

                let deadline = wall_time() + delta;
                listener!(self.update_event => listener);
                let _ = timeout_at(Some(deadline), listener).await;
            } else {
                listener!(self.update_event => listener);
                let _ = listener.await;
            }
        }
    }

}

impl FileLike for TimerFd {
    fn read(&self, dst: &mut SealedBufMut) -> AxResult<usize> {
        if dst.remaining_mut() < 8 {
            return Err(AxError::InvalidInput);
        }

        block_on(poll_io(self, IoEvents::IN, self.nonblocking(), || {
            let mut state = self.state.lock();
            if state.ticks > 0 {
                let ticks = state.ticks;
                state.ticks = 0;
                dst.write(&ticks.to_ne_bytes())?;
                Ok(8)
            } else {
                Err(AxError::WouldBlock)
            }
        }))
    }

    fn write(&self, _src: &mut SealedBuf) -> AxResult<usize> {
        Err(AxError::InvalidInput)
    }

    fn stat(&self) -> AxResult<Kstat> {
        Ok(Kstat::default())
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    fn nonblocking(&self) -> bool {
        self.non_blocking.load(Ordering::Acquire)
    }

    fn set_nonblocking(&self, flag: bool) -> AxResult<()> {
        self.non_blocking.store(flag, Ordering::Release);
        Ok(())
    }

    fn path(&self) -> Cow<str> {
        "anon_inode:[TimerFd]".into()
    }
}

impl Pollable for TimerFd {
    fn poll(&self) -> IoEvents {
        let state = self.state.lock();
        if state.ticks > 0 {
            IoEvents::IN | IoEvents::RDNORM
        } else {
            IoEvents::empty()
        }
    }

    fn register(&self, context: &mut core::task::Context<'_>, events: IoEvents) {
        if events.contains(IoEvents::IN) {
            self.poll_read.register(context.waker());
        }
    }
}