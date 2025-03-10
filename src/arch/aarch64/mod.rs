use core::marker::PhantomData;

use typenum::*;

use crate::cap::{page_state, Page, PageTable, PhantomCap};
use crate::error::{ErrorExt, SeL4Error};
use crate::vspace::{PagingRec, PagingTop};

pub mod cap;
pub mod fault;
pub mod userland;

pub type WordSize = U64;
pub type MinUntypedSize = U4;
// MaxUntypedSize is half the address space and/or word size.
pub type MaxUntypedSize = U47;
/// The number of splits it would take to extract an untyped of the minimum
/// size starting from an untyped of the maximum size
pub type MaxNaiveSplitCount = op!(MaxUntypedSize - MinUntypedSize);

/// The ASID address space is a total of 16 bits. It is bifurcated
/// into high bits and low bits where the high bits determine the
/// number of pools while the low bits identify the ASID /in/ its
/// pool.
pub type ASIDHighBits = U7;
pub type ASIDLowBits = U9;
/// The total number of available pools is 2 ^ ASIDHighBits, however,
/// there is an initial pool given to the root thread.
pub type ASIDPoolCount = op!(U1 << ASIDHighBits);
pub type ASIDPoolSize = op!(U1 << ASIDLowBits);
pub type TCBBits = U11;
pub type NotificationBits = U5;

// The paging structures are layed out as follows:
// L0: PageGlobalDirectory
// L1: |_PageUpperDirectory *L2 | HugePage
// L2:   |_PageDirectory    *L3 | LargePage
// L3:    |_PageTable
//          |_Page
pub type PageGlobalDirBits = U12;
pub type PageGlobalDirIndexBits = U9;
pub type PageUpperDirBits = U12;
pub type PageUpperDirIndexBits = U9;
pub type PageDirectoryBits = U12;
pub type PageDirIndexBits = U9;
pub type PageTableBits = U12; // How big is the kernel object for a PageTable
pub type PageTableIndexBits = U9; // How many slots are there, in addressable bit space?
pub type PageBits = U12;
pub type PageIndexBits = U12;

pub type PageBytes = op!(U1 << U12);
pub type LargePageBits = U21;
pub type HugePageBits = U30;

pub type AddressSpace = PagingRec<
    Page<page_state::Unmapped>,
    PageTable,
    PagingRec<
        PageTable,
        cap::PageDirectory,
        PagingRec<cap::PageDirectory, cap::PageUpperDirectory, PagingTop>,
    >,
>;

pub type PagingRoot = cap::PageGlobalDirectory;
/// The level directly underneath the PagingRoot
pub type PagingRootLowerLevel = cap::PageUpperDirectory;

impl AddressSpace {
    pub fn new() -> Self {
        PagingRec {
            layer: PageTable::phantom_instance(),
            next: PagingRec {
                layer: cap::PageDirectory::phantom_instance(),
                next: PagingRec {
                    layer: cap::PageUpperDirectory::phantom_instance(),
                    next: PagingTop {
                        layer: cap::PageGlobalDirectory::phantom_instance(),
                        _item: PhantomData,
                    },
                    _item: PhantomData,
                },
                _item: PhantomData,
            },
            _item: PhantomData,
        }
    }
}

pub type ARMVCPUBits = U12;

pub type BasePageDirFreeSlots = op!((U1 << PageDirectoryBits) - (U1 << U9));
pub type BasePageTableFreeSlots = op!(U1 << PageTableIndexBits);

// TODO remove these when elf stuff lands.
// this is a magic numbers we got from inspecting the binary.
/// 0x00010000
pub type ProgramStart = op!(U4 << U20);
pub type CodePageTableBits = U5;
pub type CodePageTableCount = op!(U1 << CodePageTableBits); // 32 page tables, but larger == 64 mb
pub type CodePageCount = op!(CodePageTableCount * BasePageTableFreeSlots); // 2^14
pub type TotalCodeSizeBits = op!(CodePageTableBits + PageBits + PageTableIndexBits);
pub type TotalCodeSizeBytes = crate::pow::Pow<TotalCodeSizeBits>;
// The root task has a stack size configurable by the sel4.toml
// in the `root-task-stack-bytes` metadata property.
// This configuration is turned into a generated Rust type named `RootTaskStackPageTableCount`
// that implements `typenum::Unsigned` in the `build.rs` file.
include!(concat!(
    env!("OUT_DIR"),
    "/ROOT_TASK_STACK_PAGE_TABLE_COUNT"
));
// The first N page tables are already mapped for the user image in the root
// task. Add in the stack-reserved page tables (minimum of 1 more)
pub type RootTaskReservedPageDirSlots = op!(CodePageTableCount + RootTaskStackPageTableCount);
pub type RootTaskPageDirFreeSlots = op!(BasePageDirFreeSlots - RootTaskReservedPageDirSlots);

/* EL2 has 48 addressable bits in the vaddr space, the kernel reserves
 * the top 8 of those bits.
 * 0x0000ff8000000000
 * 111111111000000000000000000000000000000000000000*/
// Cf. https://github.com/seL4/seL4/blob/c2fd4b810b18111156c8f3273d24f2ab84a06284/include/arch/arm/arch/64/mode/hardware.h#L40
#[cfg(KernelArmHypervisorSupport)]
pub type KernelReservedStart = op!(((U1 << U8) - U1) << U40);

pub const WORDS_PER_PAGE: usize = PageBytes::USIZE / core::mem::size_of::<usize>();

/// Type type alias allows us to treat vm_attributes in a cross-architecture way, abstractly
pub type VMAttributes = selfe_sys::seL4_ARM_VMAttributes;

/// A convenience module
pub mod vm_attributes {
    use super::*;

    pub const DEFAULT: VMAttributes =
        selfe_sys::seL4_ARM_VMAttributes_seL4_ARM_Default_VMAttributes;

    pub const PAGE_CACHEABLE: VMAttributes =
        selfe_sys::seL4_ARM_VMAttributes_seL4_ARM_PageCacheable;

    pub const PARITY_ENABLED: VMAttributes =
        selfe_sys::seL4_ARM_VMAttributes_seL4_ARM_ParityEnabled;

    pub const EXECUTE_NEVER: VMAttributes = selfe_sys::seL4_ARM_VMAttributes_seL4_ARM_ExecuteNever;

    pub const PROGRAM_CODE: VMAttributes = DEFAULT;

    pub const PROGRAM_DATA: VMAttributes = PAGE_CACHEABLE | PARITY_ENABLED | EXECUTE_NEVER;
}

pub(crate) unsafe fn flush_page(cptr: usize) -> Result<(), SeL4Error> {
    selfe_sys::seL4_ARM_Page_CleanInvalidate_Data(cptr, 0x0000, PageBytes::USIZE)
        .as_result()
        .map_err(|e| SeL4Error::PageCleanInvalidateData(e))?;

    Ok(())
}
