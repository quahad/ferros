//! A tiny first-chance allocator for the untyped capabilities sel4's BOOTINFO.
//! This one doesn't split anything; it just hands out the smallest untyped item
//! that's big enough for the request.

use arrayvec::ArrayVec;
use crate::userland::{role, wrap_untyped, Cap, Untyped};
use typenum::Unsigned;

use sel4_sys::{seL4_BootInfo, seL4_UntypedDesc};

pub const MIN_UNTYPED_SIZE_BITS: u8 = 4;
pub const MAX_UNTYPED_SIZE_BITS: u8 = 32;

// TODO - pull from configs
pub const MAX_INIT_UNTYPED_ITEMS: usize = 256;

struct UntypedItem {
    cptr: usize,
    desc: &'static seL4_UntypedDesc,
    is_free: bool,
}

#[derive(Debug)]
pub enum Error {
    InvalidBootInfoCapability,
    UntypedSizeOutOfRange,
}

impl UntypedItem {
    pub fn new(cptr: usize, desc: &'static seL4_UntypedDesc) -> Result<UntypedItem, Error> {
        if cptr == 0 {
            Err(Error::InvalidBootInfoCapability)
        } else if desc.sizeBits < MIN_UNTYPED_SIZE_BITS || desc.sizeBits > MAX_UNTYPED_SIZE_BITS {
            Err(Error::UntypedSizeOutOfRange)
        } else {
            Ok(UntypedItem {
                cptr,
                desc,
                is_free: true,
            })
        }
    }
}

pub struct Allocator {
    items: ArrayVec<[UntypedItem; MAX_INIT_UNTYPED_ITEMS]>,
}

impl Allocator {
    pub fn bootstrap(bootinfo: &'static seL4_BootInfo) -> Result<Allocator, Error> {
        let mut items = ArrayVec::new();
        for i in 0..(bootinfo.untyped.end - bootinfo.untyped.start) {
            match UntypedItem::new(
                (bootinfo.untyped.start + i) as usize, // cptr
                &bootinfo.untypedList[i as usize],
            ) {
                Ok(item) => items.push(item),
                Err(e) => return Err(e),
            }
        }

        Ok(Allocator { items })
    }

    fn find_block<BitSize: Unsigned>(
        &mut self,
        device_ok: bool,
        paddr: Option<usize>,
    ) -> Option<Cap<Untyped<BitSize>, role::Local>> {
        let device_byte: u8 = if device_ok { 1 } else { 0 };

        // This is very inefficient. But it should only be called a small
        // handful of times on startup.
        for bit_size in BitSize::to_u8()..=MAX_UNTYPED_SIZE_BITS {
            for item in &mut self.items {
                if (item.is_free)
                    && (item.desc.isDevice == device_byte)
                    && (item.desc.sizeBits == bit_size)
                    && match paddr {
                        Some(a) => item.desc.paddr == a,
                        None => true,
                    } {
                    let u = wrap_untyped(item.cptr, item.desc);
                    if u.is_some() {
                        item.is_free = false;
                    }
                    return u;
                }
            }
        }

        None
    }

    pub fn get_untyped<BitSize: Unsigned>(&mut self) -> Option<Cap<Untyped<BitSize>, role::Local>> {
        self.find_block::<BitSize>(false, None)
    }

    pub fn get_device_untyped<BitSize: Unsigned>(
        &mut self,
        paddr: usize,
    ) -> Option<Cap<Untyped<BitSize>, role::Local>> {
        self.find_block::<BitSize>(true, Some(paddr))
    }
}
