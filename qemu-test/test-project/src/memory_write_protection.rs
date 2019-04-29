use super::TopLevelError;
use selfe_sys::seL4_BootInfo;

use ferros::alloc::{self, micro_alloc, smart_alloc};
use typenum::*;

use ferros::userland::{retype, retype_cnode, root_cnode, BootInfo, RetypeForSetup, VSpace};

pub fn run(raw_boot_info: &'static seL4_BootInfo) -> Result<(), TopLevelError> {
    let mut allocator = micro_alloc::Allocator::bootstrap(&raw_boot_info)?;
    let (root_cnode, local_slots) = root_cnode(&raw_boot_info);

    let ut27 = allocator
        .get_untyped::<U27>()
        .expect("initial alloc failure");
    let uts = alloc::ut_buddy(ut27);

    smart_alloc!(|slots from local_slots, ut from uts| {
        let boot_info = BootInfo::wrap(raw_boot_info, ut, slots);

        let unmapped_scratch_page_table = retype(ut, slots)?;
        let (mut scratch_page_table, boot_info) =
            boot_info.map_page_table(unmapped_scratch_page_table)?;

        let (child_cnode, child_slots) = retype_cnode::<U12>(ut, slots)?;
        let params = ProcParams { value: 42 };
        let (child_vspace, mut boot_info) = VSpace::new(boot_info, ut, &root_cnode, slots)?;

        let (child_process, _) = child_vspace.prepare_thread(
            proc_main,
            params,
            ut,
            slots,
            &mut scratch_page_table,
            &mut boot_info.page_directory,
        )?;
    });

    child_process.start(child_cnode, None, &boot_info.tcb, 255)?;

    Ok(())
}

pub struct ProcParams {
    pub value: usize,
}

impl RetypeForSetup for ProcParams {
    type Output = ProcParams;
}

pub extern "C" fn proc_main(_params: ProcParams) {
    debug_println!("\nAttempting to write to the code segment...\n");

    unsafe {
        let x: *mut usize = proc_main as _;
        *x = 42;
    }

    debug_println!("This is after the segfaulting code, and should not be printed.");
}
