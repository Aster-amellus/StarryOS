use axerrno::{AxError, AxResult};
use bitflags::bitflags;
use linux_raw_sys::general::{itimerspec, O_CLOEXEC, O_NONBLOCK};

use starry_vm::{VmMutPtr, VmPtr};

use crate::file::{add_file_like, timerfd::TimerFd};

bitflags! {
    struct TimerFdFlags: u32 {
        const NONBLOCK = O_NONBLOCK;
        const CLOEXEC = O_CLOEXEC;
    }
}

pub fn sys_timerfd_create(clockid: i32, flags: u32) -> AxResult<isize> {
    let flag_parsed = TimerFdFlags::from_bits(flags).ok_or(AxError::InvalidInput)?;
    let timer = TimerFd::new(clockid, flags as _)?;

    if flag_parsed.contains(TimerFdFlags::NONBLOCK) {
        timer.set_nonblocking(true)?;
    }
    let fd = add_file_like(timer, flag_parsed.contains(TimerFdFlags::CLOEXEC))?;
    Ok(fd as isize)
}

pub fn sys_timerfd_settime(
    fd: i32,
    flags: i32,
    new_value: *const itimerspec,
    old_value: *mut itimerspec,
) -> AxResult<isize> {
    let timer = crate::file::get_file_like(fd)?;
    let timer = timer
        .into_any()
        .downcast::<TimerFd>()
        .map_err(|_| AxError::InvalidInput)?;

    if new_value.is_null() {
        return Err(AxError::InvalidInput);
    }

    let new_val = unsafe { new_value.vm_read_uninit()?.assume_init() };

    if old_value.is_null() {
        timer.set_time(flags, &new_val, None)?;
    } else {
        let mut old = unsafe { core::mem::zeroed() };
        timer.set_time(flags, &new_val, Some(&mut old))?;
        old_value.vm_write(old)?;
    }

    Ok(0)
}

pub fn sys_timerfd_gettime(fd: i32, curr_value: *mut itimerspec) -> AxResult<isize> {
    let timer = crate::file::get_file_like(fd)?;
    let timer = timer
        .into_any()
        .downcast::<TimerFd>()
        .map_err(|_| AxError::InvalidInput)?;

    let mut val = unsafe { core::mem::zeroed() };
    timer.get_time(&mut val);
    
    curr_value.vm_write(val)?;
    Ok(0)
}