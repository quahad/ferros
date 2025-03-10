use crate::arch::{self, *};
use crate::cap::*;
use crate::pow::{Pow, _Pow};
use crate::userland::rights::CapRights;
use crate::vspace::*;
use core::ops::{Add, Sub};

use selfe_sys::*;
use typenum::*;

use crate::error::{ErrorExt, SeL4Error};

use super::*;

/// A standard process in Ferros is a TCB associated with a VSpace
/// that has:
///  * A usable code image mapped/written into it.
///  * A mapped stack.
///  * Initial process state (e.g. parameter data) written into a
///    `seL4_UserContext` and/or its stack.
///  * Said seL4_UserContext written into the TCB.
///  * An IPC buffer and CSpace and fault handler associated with that
///    TCB.
pub struct StandardProcess<StackBitSize: Unsigned = DefaultStackBitSize> {
    tcb: LocalCap<ThreadControlBlock>,
    _stack_bit_size: PhantomData<StackBitSize>,
}

pub enum EntryPoint<'a, T> {
    Fork(extern "C" fn(T) -> ()),
    Elf(&'a [u8]),
}

/// If you want this to work, you need to do:
///
///     my_fn as extern "C" fn(_) -> ()
///
/// because of https://github.com/rust-lang/rust/issues/62385
impl<'a, T> From<extern "C" fn(T) -> ()> for EntryPoint<'a, T> {
    fn from(f: extern "C" fn(T) -> ()) -> Self {
        EntryPoint::Fork(f)
    }
}

impl<'a, T> From<&'a [u8]> for EntryPoint<'a, T> {
    fn from(elf_data: &'a [u8]) -> Self {
        EntryPoint::Elf(elf_data)
    }
}

impl<StackBitSize: Unsigned> StandardProcess<StackBitSize> {
    pub fn new<'a, T: RetypeForSetup, EP: Into<EntryPoint<'a, T>>>(
        vspace: &mut VSpace,
        cspace: LocalCap<ChildCNode>,
        parent_mapped_region: MappedMemoryRegion<StackBitSize, shared_status::Exclusive>,
        parent_cnode: &LocalCap<LocalCNode>,
        entry_point: EP,
        process_parameter: SetupVer<T>,
        ipc_buffer_ut: LocalCap<Untyped<PageBits>>,
        tcb_ut: LocalCap<Untyped<<ThreadControlBlock as DirectRetype>::SizeBits>>,
        slots: LocalCNodeSlots<Sum<NumPages<StackBitSize>, U2>>,
        priority_authority: &LocalCap<ThreadPriorityAuthority>,
        fault_source: Option<crate::userland::FaultSource<role::Child>>,
    ) -> Result<StandardProcess<StackBitSize>, ProcessSetupError>
    where
        NumPages<StackBitSize>: Add<U2>,
        Sum<NumPages<StackBitSize>, U2>: Unsigned,

        Sum<NumPages<StackBitSize>, U2>: Sub<U2>,
        Diff<Sum<NumPages<StackBitSize>, U2>, U2>: Unsigned,
        Diff<Sum<NumPages<StackBitSize>, U2>, U2>: IsEqual<NumPages<StackBitSize>, Output = True>,

        StackBitSize: IsGreaterOrEqual<PageBits>,
        StackBitSize: Sub<PageBits>,
        <StackBitSize as Sub<PageBits>>::Output: Unsigned,
        <StackBitSize as Sub<PageBits>>::Output: _Pow,
        Pow<<StackBitSize as Sub<PageBits>>::Output>: Unsigned,
    {
        let entry_point = entry_point.into();

        if parent_mapped_region.asid() == vspace.asid() {
            return Err(
                ProcessSetupError::ParentMappedMemoryRegionASIDShouldNotMatchChildVSpaceASID,
            );
        }

        let (misc_slots, stack_slots) = slots.alloc::<U2>();
        // TODO - lift these checks to compile-time, as static assertions
        // Note - This comparison is conservative because technically
        // we can fit some of the params into available registers.
        if core::mem::size_of::<SetupVer<T>>() > 2usize.pow(StackBitSize::U32) {
            return Err(ProcessSetupError::ProcessParameterTooBigForStack);
        }
        if core::mem::size_of::<SetupVer<T>>() != core::mem::size_of::<T>() {
            return Err(ProcessSetupError::ProcessParameterHandoffSizeMismatch);
        }

        // Reserve a guard page before the stack
        vspace.skip_pages(1)?;

        // Map the stack to the target address space
        let stack_top = parent_mapped_region.vaddr() + parent_mapped_region.size_bytes();
        let (unmapped_stack_pages, local_stack_pages): (UnmappedMemoryRegion<StackBitSize, _>, _) =
            parent_mapped_region.share(stack_slots, parent_cnode, CapRights::RW)?;
        let mapped_stack_pages = vspace.map_shared_region_and_consume(
            unmapped_stack_pages,
            CapRights::RW,
            arch::vm_attributes::DEFAULT | arch::vm_attributes::EXECUTE_NEVER,
        )?;

        // map the child stack into local memory so we can copy the contents
        // of the process params into it
        let (mut registers, param_size_on_stack) = unsafe {
            setup_initial_stack_and_regs(
                &process_parameter as *const SetupVer<T> as *const usize,
                core::mem::size_of::<SetupVer<T>>(),
                stack_top as *mut usize,
                mapped_stack_pages.vaddr() + mapped_stack_pages.size_bytes(),
            )
        };

        local_stack_pages.flush()?;

        let stack_pointer =
            mapped_stack_pages.vaddr() + mapped_stack_pages.size_bytes() - param_size_on_stack;

        registers.sp = stack_pointer;

        registers.pc = match entry_point {
            EntryPoint::Fork(f) => f as usize,
            EntryPoint::Elf(elf_data) => {
                let elf =
                    xmas_elf::ElfFile::new(elf_data).map_err(ProcessSetupError::ElfParseError)?;
                elf.header.pt2.entry_point() as usize
            }
        };

        // TODO - Probably ought to suspend or destroy the thread instead of endlessly yielding
        match entry_point {
            // This doesn't work for elf procs, since yield_forever isn't there
            EntryPoint::Fork(_) => {
                set_thread_link_register(&mut registers, yield_forever);
                ()
            }
            _ => (),
        };

        // Reserve a guard page after the stack
        vspace.skip_pages(1)?;

        // Allocate and map the ipc buffer
        let (ipc_slots, misc_slots) = misc_slots.alloc();
        let ipc_buffer = ipc_buffer_ut.retype(ipc_slots)?;
        let ipc_buffer = vspace.map_region(
            ipc_buffer.to_region(),
            CapRights::RW,
            arch::vm_attributes::DEFAULT | arch::vm_attributes::EXECUTE_NEVER,
        )?;

        //// allocate the thread control block
        let (tcb_slots, _slots) = misc_slots.alloc();
        let mut tcb = tcb_ut.retype(tcb_slots)?;

        tcb.configure(
            cspace,
            fault_source,
            &vspace.root(),
            Some(ipc_buffer.to_page()),
        )?;
        unsafe {
            seL4_TCB_WriteRegisters(
                tcb.cptr,
                0,
                0,
                // all the regs
                core::mem::size_of::<seL4_UserContext>() / core::mem::size_of::<usize>(),
                &mut registers,
            )
            .as_result()
            .map_err(|e| ProcessSetupError::SeL4Error(SeL4Error::TCBWriteRegisters(e)))?;

            // TODO - priority management could be exposed once we
            // plan on actually using it
            tcb.set_priority(priority_authority, 255)?;
        }
        Ok(StandardProcess {
            tcb,
            _stack_bit_size: PhantomData,
        })
    }

    pub fn set_name(&mut self, name: &str) {
        let mut c_str = [0u8; 256];
        for (n, byte) in name.bytes().take(255).enumerate() {
            c_str[n] = byte;
        }

        unsafe {
            seL4_DebugNameThread(self.tcb.cptr, &c_str as *const u8 as *const i8);
        }
    }

    pub fn bind_notification(
        &mut self,
        notification: &LocalCap<Notification>,
    ) -> Result<(), SeL4Error> {
        unsafe { seL4_TCB_BindNotification(self.tcb.cptr, notification.cptr) }
            .as_result()
            .map_err(|e| SeL4Error::TCBBindNotification(e))
    }

    pub fn start(&mut self) -> Result<(), SeL4Error> {
        unsafe { seL4_TCB_Resume(self.tcb.cptr) }
            .as_result()
            .map_err(|e| SeL4Error::TCBResume(e))
    }

    pub fn elim(self) -> usize {
        self.tcb.cptr
    }

    pub fn unsafe_get_tcb_cptr(&self) -> usize {
        self.tcb.cptr
    }
}
