use crate::error::{ErrorExt, SeL4Error};
use typenum::*;

pub mod cap;
pub mod fault;
pub mod userland;

pub type WordSize = U32;
pub type MinUntypedSize = U4;
// MaxUntypedSize is half the address space and/or word size.
pub type MaxUntypedSize = U29;
/// The number of splits it would take to extract an untyped of the minimum
/// size starting from an untyped of the maximum size
pub type MaxNaiveSplitCount = op!(MaxUntypedSize - MinUntypedSize);

/// The ASID address space is a total of 16 bits. It is bifurcated
/// into high bits and low bits where the high bits determine the
/// number of pools while the low bits identify the ASID /in/ its
/// pool.
pub type ASIDHighBits = U6;
pub type ASIDLowBits = U10;
/// The total number of available pools is 2 ^ ASIDHighBits, however,
/// there is an initial pool given to the root thread.
pub type ASIDPoolCount = op!((U1 << ASIDHighBits) - U1);
pub type ASIDPoolSize = op!(U1 << ASIDLowBits);
pub type TCBBits = U10;
pub type NotificationBits = U4;

#[cfg(KernelHypervisorSupport)]
mod hyp_dependent_constants {
    use typenum::*;
    pub type PGDBits = U5;
    pub type PGDIndexBits = U2;
    pub type PageTableBits = U12;
    pub type PageTableIndexBits = U9;
    pub type PageDirIndexBits = U11;
    pub type VCPUBits = U12;
    pub type SectionBits = U21;
    pub type SuperSectionBits = U25;
}

#[cfg(not(KernelHypervisorSupport))]
mod hyp_dependent_constants {
    use core::marker::PhantomData;

    use crate::cap::{page_state, Page, PageTable};
    use crate::vspace::{PagingRec, PagingTop};

    use typenum::*;

    use super::cap;

    pub type PageTableBits = U10;
    pub type PageTableIndexBits = U8;
    pub type PageDirIndexBits = U12;
    pub type SectionBits = U20;
    pub type SuperSectionBits = U24;

    pub type AddressSpace = PagingRec<Page<page_state::Unmapped>, PageTable, PagingTop>;

    pub type PagingRoot = cap::PageDirectory;
    /// The level directly underneath the PagingRoot
    pub type PagingRootLowerLevel = PageTable;

    impl AddressSpace {
        pub fn new() -> Self {
            PagingRec {
                layer: PageTable {},
                next: PagingTop {
                    layer: cap::PageDirectory {},
                    _item: PhantomData,
                },
                _item: PhantomData,
            }
        }
    }
}

pub use hyp_dependent_constants::*;

pub type PageDirectoryBits = U14;
pub type PageBits = U12;
pub type PageIndexBits = U12;
pub type PageBytes = op!(U1 << U12);
pub type LargePageBits = U16;

pub type BasePageDirFreeSlots = op!((U1 << PageDirIndexBits) - (U1 << U9));
pub type BasePageTableFreeSlots = op!(U1 << PageTableIndexBits);

// TODO remove these when elf stuff lands.
// this is a magic number we got from inspecting the binary.
/// 0x00010000
pub type ProgramStart = op!(U1 << U16);
pub type CodePageTableBits = U6;
pub type CodePageTableCount = op!(U1 << CodePageTableBits); // 64 page tables == 64 mb
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

/// 0xe0000000
pub type KernelReservedStart = op!((U1 << U32) - U1);

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
