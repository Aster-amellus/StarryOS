use alloc::vec::Vec;
use core::{fmt, mem, ptr, time::Duration};

use axerrno::{AxError, AxResult};
use axpoll::IoEvents;
use axtask::future::{self, block_on, poll_io};
use bitmaps::Bitmap;
use linux_raw_sys::{
    general::*,
    select_macros::{FD_ISSET, FD_SET, FD_ZERO},
};
use starry_core::mm::access_user_memory;
use starry_signal::SignalSet;

use super::FdPollSet;
use crate::{
    file::FD_TABLE,
    mm::{UserConstPtr, UserPtr, nullable},
    signal::with_replacen_blocked,
    syscall::signal::check_sigset_size,
    time::TimeValueLike,
};

struct FdSet(Bitmap<{ __FD_SETSIZE as usize }>);

impl FdSet {
    fn new(nfds: usize, fds: Option<&__kernel_fd_set>) -> Self {
        let mut bitmap = Bitmap::new();
        if let Some(fds) = fds {
            for i in 0..nfds {
                if unsafe { FD_ISSET(i as _, fds) } {
                    bitmap.set(i, true);
                }
            }
        }
        Self(bitmap)
    }
}

fn load_fdset(ptr: UserPtr<__kernel_fd_set>) -> AxResult<Option<__kernel_fd_set>> {
    if ptr.is_null() {
        return Ok(None);
    }
    let set_ref = ptr.get_as_mut()?;
    // Touching user memory may fault; make it recoverable.
    let val = access_user_memory(|| -> AxResult<__kernel_fd_set> {
        Ok(unsafe { ptr::read(set_ref) })
    })?;
    Ok(Some(val))
}

fn store_fdset(ptr: UserPtr<__kernel_fd_set>, value: &__kernel_fd_set) -> AxResult<()> {
    if ptr.is_null() {
        return Ok(());
    }
    let set_ref = ptr.get_as_mut()?;
    access_user_memory(|| -> AxResult<()> {
        unsafe {
            ptr::write(set_ref, ptr::read(value));
        }
        Ok(())
    })
}

impl fmt::Debug for FdSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(&self.0).finish()
    }
}

fn do_select(
    nfds: u32,
    readfds: UserPtr<__kernel_fd_set>,
    writefds: UserPtr<__kernel_fd_set>,
    exceptfds: UserPtr<__kernel_fd_set>,
    timeout: Option<Duration>,
    sigmask: UserConstPtr<SignalSetWithSize>,
) -> AxResult<isize> {
    if nfds > __FD_SETSIZE {
        return Err(AxError::InvalidInput);
    }
    let sigmask = if let Some(sigmask) = nullable!(sigmask.get_as_ref())? {
        check_sigset_size(sigmask.sigsetsize)?;
        let set = sigmask.set;
        nullable!(set.get_as_ref())?
    } else {
        None
    };

    // Load input fd_sets into kernel memory. Writing user memory directly from the
    // kernel may trigger a supervisor-mode page fault that won't be handled unless
    // it's wrapped by `access_user_memory`.
    let readfds_in = load_fdset(readfds)?;
    let writefds_in = load_fdset(writefds)?;
    let exceptfds_in = load_fdset(exceptfds)?;

    let read_set = FdSet::new(nfds as _, readfds_in.as_ref());
    let write_set = FdSet::new(nfds as _, writefds_in.as_ref());
    let except_set = FdSet::new(nfds as _, exceptfds_in.as_ref());

    debug!(
        "sys_select <= nfds: {nfds} sets: [read: {read_set:?}, write: {write_set:?}, except: \
         {except_set:?}] timeout: {timeout:?}"
    );

    let fd_table = FD_TABLE.read();
    let fd_bitmap = read_set.0 | write_set.0 | except_set.0;
    let fd_count = fd_bitmap.len();
    let mut fds = Vec::with_capacity(fd_count);
    let mut fd_indices = Vec::with_capacity(fd_count);
    for fd in fd_bitmap.into_iter() {
        let f = fd_table
            .get(fd)
            .ok_or(AxError::BadFileDescriptor)?
            .inner
            .clone();
        let mut events = IoEvents::empty();
        events.set(IoEvents::IN, read_set.0.get(fd));
        events.set(IoEvents::OUT, write_set.0.get(fd));
        events.set(IoEvents::ERR, except_set.0.get(fd));
        if !events.is_empty() {
            fds.push((f, events));
            fd_indices.push(fd);
        }
    }

    drop(fd_table);
    let fds = FdPollSet(fds);

    let ready_count: isize = with_replacen_blocked(sigmask.copied(), || {
        match block_on(future::timeout(
            timeout,
            poll_io(&fds, IoEvents::empty(), false, || {
                // Only decide readiness here. Do NOT touch user fd_sets in this polling loop.
                let mut res = 0usize;
                for (fd, interested) in fds.0.iter().map(|(f, e)| (f, e)) {
                    if !(fd.poll() & *interested).is_empty() {
                        res += 1;
                    }
                }
                if res > 0 {
                    return Ok(res as _);
                }
                Err(AxError::WouldBlock)
            }),
        )) {
            Ok(r) => r,
            Err(_) => Ok(0),
        }
    })?;

    // Build output fd_sets in kernel memory.
    let mut out_read: __kernel_fd_set = unsafe { mem::zeroed() };
    let mut out_write: __kernel_fd_set = unsafe { mem::zeroed() };
    let mut out_except: __kernel_fd_set = unsafe { mem::zeroed() };
    unsafe {
        FD_ZERO(&mut out_read);
        FD_ZERO(&mut out_write);
        FD_ZERO(&mut out_except);
    }

    if ready_count > 0 {
        let mut res = 0isize;
        for ((fd, interested), index) in fds.0.iter().zip(fd_indices.iter().copied()) {
            let events = fd.poll() & *interested;
            if events.is_empty() {
                continue;
            }
            res += 1;
            if events.contains(IoEvents::IN) {
                unsafe { FD_SET(index as _, &mut out_read) };
            }
            if events.contains(IoEvents::OUT) {
                unsafe { FD_SET(index as _, &mut out_write) };
            }
            if events.contains(IoEvents::ERR) {
                unsafe { FD_SET(index as _, &mut out_except) };
            }
        }

        store_fdset(readfds, &out_read)?;
        store_fdset(writefds, &out_write)?;
        store_fdset(exceptfds, &out_except)?;
        Ok(res)
    } else {
        // Timeout: select returns 0 and clears all sets.
        store_fdset(readfds, &out_read)?;
        store_fdset(writefds, &out_write)?;
        store_fdset(exceptfds, &out_except)?;
        Ok(0)
    }
}

#[cfg(target_arch = "x86_64")]
pub fn sys_select(
    nfds: u32,
    readfds: UserPtr<__kernel_fd_set>,
    writefds: UserPtr<__kernel_fd_set>,
    exceptfds: UserPtr<__kernel_fd_set>,
    timeout: UserConstPtr<timeval>,
) -> AxResult<isize> {
    do_select(
        nfds,
        readfds,
        writefds,
        exceptfds,
        nullable!(timeout.get_as_ref())?
            .map(|it| it.try_into_time_value())
            .transpose()?,
        0.into(),
    )
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SignalSetWithSize {
    set: UserConstPtr<SignalSet>,
    sigsetsize: usize,
}

pub fn sys_pselect6(
    nfds: u32,
    readfds: UserPtr<__kernel_fd_set>,
    writefds: UserPtr<__kernel_fd_set>,
    exceptfds: UserPtr<__kernel_fd_set>,
    timeout: UserConstPtr<timespec>,
    sigmask: UserConstPtr<SignalSetWithSize>,
) -> AxResult<isize> {
    do_select(
        nfds,
        readfds,
        writefds,
        exceptfds,
        nullable!(timeout.get_as_ref())?
            .map(|ts| ts.try_into_time_value())
            .transpose()?,
        sigmask,
    )
}
