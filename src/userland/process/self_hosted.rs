use typenum::*;

use selfe_sys::*;

use crate::arch::{self, PageBits};
use crate::cap::{
    role, CNodeRole, CNodeSlotsError, Cap, ChildCNode, DirectRetype, LocalCNode, LocalCNodeSlots,
    LocalCap, ThreadControlBlock, ThreadPriorityAuthority, Untyped, WCNodeSlotsData,
};
use crate::userland::CapRights;
use crate::vspace::*;

use super::*;

pub struct SelfHostedProcess {
    tcb: LocalCap<ThreadControlBlock>,
}

struct SelfHostedParams<T, Role: CNodeRole> {
    params: T,
    vspace: VSpace<vspace_state::Imaged, Role>,
    child_main: extern "C" fn(VSpace<vspace_state::Imaged, role::Local>, T) -> (),
}

extern "C" fn self_hosted_run<T>(sh_params: SelfHostedParams<T, role::Local>) {
    let SelfHostedParams {
        params,
        vspace,
        child_main,
    } = sh_params;
    child_main(vspace, params);
}

impl SelfHostedProcess {
    pub fn new<T: RetypeForSetup>(
        mut vspace: VSpace<vspace_state::Imaged, role::Local>,
        cspace: LocalCap<ChildCNode>,
        parent_mapped_region: MappedMemoryRegion<StackBitSize, shared_status::Exclusive>,
        parent_cnode: &LocalCap<LocalCNode>,
        function_descriptor: extern "C" fn(VSpace<vspace_state::Imaged, role::Local>, T) -> (),
        process_parameter: SetupVer<T>,
        ipc_buffer_ut: LocalCap<Untyped<PageBits>>,
        tcb_ut: LocalCap<Untyped<<ThreadControlBlock as DirectRetype>::SizeBits>>,
        slots: LocalCNodeSlots<PrepareThreadCNodeSlots>,
        mut cap_transfer_slots: LocalCap<WCNodeSlotsData<role::Child>>,
        child_paging_slots: Cap<WCNodeSlotsData<role::Child>, role::Child>,
        priority_authority: &LocalCap<ThreadPriorityAuthority>,
        fault_source: Option<crate::userland::FaultSource<role::Child>>,
    ) -> Result<Self, ProcessSetupError> {
        // TODO - lift these checks to compile-time, as static assertions
        // Note - This comparison is conservative because technically
        // we can fit some of the params into available registers.
        if core::mem::size_of::<SelfHostedParams<SetupVer<T>, role::Child>>()
            > (StackPageCount::USIZE * arch::PageBytes::USIZE)
        {
            return Err(ProcessSetupError::ProcessParameterTooBigForStack);
        }
        if core::mem::size_of::<SelfHostedParams<SetupVer<T>, role::Child>>()
            != core::mem::size_of::<SelfHostedParams<T, role::Child>>()
        {
            return Err(ProcessSetupError::ProcessParameterHandoffSizeMismatch);
        }

        // Allocate and map the ipc buffer
        let (ipc_slots, slots) = slots.alloc();
        let ipc_buffer = ipc_buffer_ut.retype(ipc_slots)?;
        let ipc_buffer = vspace.map_given_page(
            ipc_buffer,
            CapRights::RW,
            arch::vm_attributes::DEFAULT & arch::vm_attributes::EXECUTE_NEVER,
        )?;

        // allocate the thread control block
        let (tcb_slots, slots) = slots.alloc();
        let mut tcb = tcb_ut.retype(tcb_slots)?;

        tcb.configure(cspace, fault_source, &vspace, ipc_buffer)?;

        // Reserve a guard page before the stack
        vspace.skip_pages(1)?;

        // Map the stack to the target address space
        let stack_top = parent_mapped_region.vaddr() + parent_mapped_region.size();
        let (page_slots, _slots) = slots.alloc();
        let (unmapped_stack_pages, _) =
            parent_mapped_region.share(page_slots, parent_cnode, CapRights::RW)?;
        let mapped_stack_pages = vspace.map_shared_region_and_consume(
            unmapped_stack_pages,
            CapRights::RW,
            arch::vm_attributes::DEFAULT & arch::vm_attributes::EXECUTE_NEVER,
        )?;

        // Reserve a guard page after the stack.
        vspace.skip_pages(1)?;

        let root_slot = cap_transfer_slots.alloc_strong().map_err(|e| match e {
            CNodeSlotsError::NotEnoughSlots => ProcessSetupError::NotEnoughCNodeSlots,
        })?;
        let child_vspace = vspace.for_child(
            parent_cnode,
            root_slot,
            cap_transfer_slots,
            child_paging_slots,
        )?;

        let sh_params = SelfHostedParams {
            vspace: child_vspace,
            params: process_parameter,
            child_main: unsafe { core::mem::transmute(function_descriptor) },
        };

        // map the child stack into local memory so we can copy the contents
        // of the process params into it
        let (mut registers, param_size_on_stack) = unsafe {
            setup_initial_stack_and_regs(
                &sh_params as *const SelfHostedParams<SetupVer<T>, role::Child> as *const usize,
                core::mem::size_of::<SelfHostedParams<SetupVer<T>, role::Child>>(),
                stack_top as *mut usize,
                mapped_stack_pages.vaddr() + mapped_stack_pages.size(),
            )
        };

        let stack_pointer =
            mapped_stack_pages.vaddr() + mapped_stack_pages.size() - param_size_on_stack;

        registers.sp = stack_pointer;
        registers.pc = self_hosted_run::<T> as usize;

        // TODO - Probably ought to suspend or destroy the thread
        // instead of endlessly yielding
        set_thread_link_register(&mut registers, yield_forever);

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
            seL4_TCB_SetPriority(tcb.cptr, priority_authority.cptr, 255)
                .as_result()
                .map_err(|e| ProcessSetupError::SeL4Error(SeL4Error::TCBSetPriority(e)))?;
        }
        Ok(SelfHostedProcess { tcb })
    }

    pub fn start(self) -> Result<(), SeL4Error> {
        unsafe { seL4_TCB_Resume(self.tcb.cptr) }
            .as_result()
            .map_err(|e| SeL4Error::TCBResume(e))
    }
}
