#![no_std]

extern crate iron_pegasus;
extern crate sel4_sys;
extern crate typenum;

use sel4_sys::*;

macro_rules! debug_print {
    ($($arg:tt)*) => ({
        use core::fmt::Write;
        DebugOutHandle.write_fmt(format_args!($($arg)*)).unwrap();
    });
}

macro_rules! debug_println {
    ($fmt:expr) => (debug_print!(concat!($fmt, "\n")));
    ($fmt:expr, $($arg:tt)*) => (debug_print!(concat!($fmt, "\n"), $($arg)*));
}

#[cfg(dual_process = "true")]
mod dual_process;
#[cfg(single_process = "true")]
mod single_process;

use iron_pegasus::micro_alloc::{self};
use iron_pegasus::pow::Pow;
use iron_pegasus::userland::{
    role, root_cnode, spawn, BootInfo, CNode, CNodeRole, Cap, Endpoint, LocalCap, RetypeForSetup,
    Untyped,
};
use typenum::operator_aliases::Diff;
use typenum::{U12, U2, U20, U4096, U6};

fn yield_forever() {
    unsafe {
        loop {
            seL4_Yield();
        }
    }
}

pub fn run(raw_boot_info: &'static seL4_BootInfo) {
    #[cfg(single_process = "true")]
    single_process::run(raw_boot_info);
    #[cfg(dual_process = "true")]
    dual_process::run(raw_boot_info);

    yield_forever()
}
