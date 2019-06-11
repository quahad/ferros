//! A VSpace represents the virtual address space of a process in
//! seL4.
//!
//! This architecture-independent realization of that concept uses
//! memory _regions_ rather than expose the granules that each layer
//! in the addressing structures is responsible for mapping.
use core::marker::PhantomData;
use core::ops::Sub;

use typenum::*;

use selfe_sys::*;

use crate::arch::cap::{page_state, AssignedASID, Page, UnassignedASID};
use crate::arch::{AddressSpace, PageBits, PageBytes, PagingRoot};
use crate::bootstrap::UserImage;
use crate::cap::{
    role, Cap, CapRange, CapType, DirectRetype, LocalCNode, LocalCNodeSlots, LocalCap, PhantomCap,
    RetypeError, Untyped, WCNodeSlots, WCNodeSlotsData, WUntyped,
};
use crate::error::SeL4Error;
use crate::pow::{Pow, _Pow};
use crate::userland::CapRights;

pub trait SharedStatus: private::SealedSharedStatus {}

pub mod shared_status {
    use super::SharedStatus;

    pub struct Shared;
    impl SharedStatus for Shared {}

    pub struct Exclusive;
    impl SharedStatus for Exclusive {}
}

pub trait VSpaceState: private::SealedVSpaceState {}

pub mod vspace_state {
    use super::VSpaceState;

    pub struct Empty;
    impl VSpaceState for Empty {}

    pub struct Imaged;
    impl VSpaceState for Imaged {}
}

/// A `Maps` implementor is a paging layer that maps granules of type
/// `G`. The if this layer isn't present for the incoming address,
/// `MappingError::Overflow` should be returned, as this signals to
/// the caller—the layer above—that it needs to create a new object at
/// this layer and then attempt again to map the `item`.
pub trait Maps<G: CapType> {
    fn map_item<RootG: CapType, Root>(
        &mut self,
        item: &LocalCap<G>,
        addr: usize,
        root: &mut LocalCap<Root>,
        rights: CapRights,
        ut: &mut WUntyped,
        slots: &mut WCNodeSlots,
    ) -> Result<(), MappingError>
    where
        Root: Maps<RootG>,
        Root: CapType;
}

#[derive(Debug)]
/// The error type returned when there is in an error in the
/// construction of any of the intermediate layers of the paging
/// structure.
pub enum MappingError {
    /// Overflow is the special variant that signals to the caller
    /// that this layer is missing and the intermediate-layer mapping
    /// ought to roll up an additional layer.
    Overflow,
    AddrNotPageAligned,
    /// In all seL4-support architectures, a page is the smallest
    /// granule; it aligns with a physical frame of memory. This error
    /// is broken out to differentiate between a failure at the leaf
    /// rather than during branch construction.
    PageMapFailure(SeL4Error),
    /// A failure to map one of the intermediate layers.
    IntermediateLayerFailure(SeL4Error),
    /// The error was specific to retyping the untyped memory the
    /// layers thread through during their mapping. This likely
    /// signals that this VSpace is out of resources with which to
    /// convert to intermediate structures.
    RetypingError,
}

#[derive(Debug)]
/// The error type returned by VSpace operations.
pub enum VSpaceError {
    /// An error occurred when mapping a region.
    MappingError(MappingError),
    /// An error occurred when retyping a region to an
    /// `UnmappedMemoryRegion`.
    RetypeRegion(RetypeError),
    /// A wrapper around the top-level syscall error type.
    SeL4Error(SeL4Error),
    /// There are no more slots in which to place retyped layer caps.
    InsufficientCNodeSlots,
    ExceededAvailableAddressSpace,
}

impl From<RetypeError> for VSpaceError {
    fn from(e: RetypeError) -> VSpaceError {
        VSpaceError::RetypeRegion(e)
    }
}

impl From<SeL4Error> for VSpaceError {
    fn from(e: SeL4Error) -> VSpaceError {
        VSpaceError::SeL4Error(e)
    }
}

/// A `PagingLayer` is a mapping-layer in an architecture's address
/// space structure.
pub trait PagingLayer {
    /// The `Item` is the granule which this layer maps.
    type Item: DirectRetype + CapType;

    /// A function which attempts to map this layer's granule at the
    /// given address. If the error is a seL4 lookup error, then the
    /// implementor ought to return `MappingError::Overflow` to signal
    /// that mapping is needed at the layer above, otherwise the error
    /// is just bubbled up to the caller.
    fn map_item<RootG: CapType, Root>(
        &mut self,
        item: &LocalCap<Self::Item>,
        addr: usize,
        root: &mut LocalCap<Root>,
        rights: CapRights,
        ut: &mut WUntyped,
        slots: &mut WCNodeSlots,
    ) -> Result<(), MappingError>
    where
        Root: Maps<RootG>,
        Root: CapType;
}

/// `PagingTop` represents the root of an address space structure.
pub struct PagingTop<G, L: Maps<G>>
where
    L: CapType,
    G: CapType,
{
    pub layer: L,
    pub(super) _item: PhantomData<G>,
}

impl<G, L: Maps<G>> PagingLayer for PagingTop<G, L>
where
    L: CapType,
    G: DirectRetype,
    G: CapType,
{
    type Item = G;
    fn map_item<RootG: CapType, Root>(
        &mut self,
        item: &LocalCap<G>,
        addr: usize,
        root: &mut LocalCap<Root>,
        rights: CapRights,
        ut: &mut WUntyped,
        slots: &mut WCNodeSlots,
    ) -> Result<(), MappingError>
    where
        Root: Maps<RootG>,
        Root: CapType,
    {
        self.layer.map_item(item, addr, root, rights, ut, slots)
    }
}

/// `PagingRec` represents an intermediate layer. It is of type `L`,
/// while it maps `G`s. The layer above it is inside `P`.
pub struct PagingRec<G: CapType, L: Maps<G>, P: PagingLayer> {
    pub(crate) layer: L,
    pub(crate) next: P,
    pub(crate) _item: PhantomData<G>,
}

impl<G, L: Maps<G>, P: PagingLayer> PagingLayer for PagingRec<G, L, P>
where
    L: CapType,
    G: DirectRetype,
    G: CapType,
{
    type Item = G;
    fn map_item<RootG: CapType, Root>(
        &mut self,
        item: &LocalCap<G>,
        addr: usize,
        root: &mut LocalCap<Root>,
        rights: CapRights,
        ut: &mut WUntyped,
        mut slots: &mut WCNodeSlots,
    ) -> Result<(), MappingError>
    where
        Root: Maps<RootG>,
        Root: CapType,
    {
        match self.layer.map_item(item, addr, root, rights, ut, slots) {
            Err(MappingError::Overflow) => {
                let next_item = match ut.retype::<P::Item>(&mut slots) {
                    Ok(i) => i,
                    Err(_) => return Err(MappingError::RetypingError),
                };
                self.next
                    .map_item(&next_item, addr, root, rights, ut, slots)?;
                self.layer.map_item(item, addr, root, rights, ut, slots)
            }
            res => res,
        }
    }
}

type NumPages<Size> = Pow<op!(Size - PageBits)>;

/// A `1 << SizeBits` bytes region of unmapped memory. It can be
/// shared or owned exclusively. The ramifications of its shared
/// status are described more completely in the `mapped_shared_region`
/// function description.
pub struct UnmappedMemoryRegion<SizeBits: Unsigned, SS: SharedStatus>
where
    // Forces regions to be page-aligned.
    SizeBits: IsGreaterOrEqual<PageBits>,
    SizeBits: Sub<PageBits>,
    <SizeBits as Sub<PageBits>>::Output: Unsigned,
    <SizeBits as Sub<PageBits>>::Output: _Pow,
    Pow<<SizeBits as Sub<PageBits>>::Output>: Unsigned,
{
    caps: CapRange<Page<page_state::Unmapped>, role::Local, NumPages<SizeBits>>,
    _size_bits: PhantomData<SizeBits>,
    _shared_status: PhantomData<SS>,
}

impl<SizeBits: Unsigned, SS: SharedStatus> UnmappedMemoryRegion<SizeBits, SS>
where
    SizeBits: IsGreaterOrEqual<PageBits>,
    SizeBits: Sub<PageBits>,
    <SizeBits as Sub<PageBits>>::Output: Unsigned,
    <SizeBits as Sub<PageBits>>::Output: _Pow,
    Pow<<SizeBits as Sub<PageBits>>::Output>: Unsigned,
{
    /// The size of this region in bytes.
    pub const SIZE_BYTES: usize = 1 << SizeBits::USIZE;
}

impl<SizeBits: Unsigned> UnmappedMemoryRegion<SizeBits, shared_status::Exclusive>
where
    SizeBits: IsGreaterOrEqual<PageBits>,
    SizeBits: Sub<PageBits>,
    <SizeBits as Sub<PageBits>>::Output: Unsigned,
    <SizeBits as Sub<PageBits>>::Output: _Pow,
    Pow<<SizeBits as Sub<PageBits>>::Output>: Unsigned,
{
    /// Retype the necessary number of granules into memory
    /// capabilities and return the unmapped region.
    pub(crate) fn new(
        ut: LocalCap<Untyped<SizeBits>>,
        slots: LocalCNodeSlots<NumPages<SizeBits>>,
    ) -> Result<Self, VSpaceError> {
        let page_caps =
            ut.retype_multi_runtime::<Page<page_state::Unmapped>, NumPages<SizeBits>>(slots)?;
        Ok(UnmappedMemoryRegion {
            caps: CapRange::new(page_caps.start_cptr),
            _size_bits: PhantomData,
            _shared_status: PhantomData,
        })
    }

    pub(crate) fn size(&self) -> usize {
        self.caps.len() * PageBytes::USIZE
    }

    /// A shared region of memory can be duplicated. When it is
    /// mapped, it's _borrowed_ rather than consumed allowing for its
    /// remapping into other address spaces.
    pub fn to_shared(self) -> UnmappedMemoryRegion<SizeBits, shared_status::Shared> {
        UnmappedMemoryRegion {
            caps: CapRange::new(self.caps.start_cptr),
            _size_bits: PhantomData,
            _shared_status: PhantomData,
        }
    }
}

struct MappedPageRange<Count: Unsigned> {
    initial_cptr: usize,
    initial_vaddr: usize,
    asid: u32,
    _count: PhantomData<Count>,
}

impl<Count: Unsigned> MappedPageRange<Count> {
    fn new(initial_cptr: usize, initial_vaddr: usize, asid: u32) -> Self {
        MappedPageRange {
            initial_cptr,
            initial_vaddr,
            asid,
            _count: PhantomData,
        }
    }

    pub(crate) fn size(&self) -> usize {
        Count::USIZE * PageBytes::USIZE
    }

    pub fn iter(self) -> impl Iterator<Item = Cap<Page<page_state::Mapped>, role::Local>> {
        (0..Count::USIZE).map(move |idx| Cap {
            cptr: self.initial_cptr + idx,
            cap_data: Page {
                state: page_state::Mapped {
                    vaddr: self.initial_vaddr + (PageBytes::USIZE * idx),
                    asid: self.asid,
                },
            },
            _role: PhantomData,
        })
    }
}

/// A memory region which is mapped into an address space, meaning it
/// has a virtual address and an associated asid in which that virtual
/// address is valid.
///
/// The distinction between its shared-or-not-shared status is to
/// prevent an unwitting unmap into an `UnmappedMemoryRegion` which
/// loses the sharededness context.
pub struct MappedMemoryRegion<SizeBits: Unsigned, SS: SharedStatus>
where
    SizeBits: IsGreaterOrEqual<PageBits>,
    SizeBits: Sub<PageBits>,
    <SizeBits as Sub<PageBits>>::Output: Unsigned,
    <SizeBits as Sub<PageBits>>::Output: _Pow,
    Pow<<SizeBits as Sub<PageBits>>::Output>: Unsigned,
{
    pub vaddr: usize,
    caps: MappedPageRange<NumPages<SizeBits>>,
    asid: u32,
    _size_bits: PhantomData<SizeBits>,
    _shared_status: PhantomData<SS>,
}

impl<SizeBits: Unsigned, SS: SharedStatus> MappedMemoryRegion<SizeBits, SS>
where
    SizeBits: IsGreaterOrEqual<PageBits>,
    SizeBits: Sub<PageBits>,
    <SizeBits as Sub<PageBits>>::Output: Unsigned,
    <SizeBits as Sub<PageBits>>::Output: _Pow,
    Pow<<SizeBits as Sub<PageBits>>::Output>: Unsigned,
{
    pub(crate) fn size(&self) -> usize {
        self.caps.size()
    }
}

pub enum ProcessCodeImageConfig {
    ReadOnly,
    /// Use when you need to be able to write to statics in the child process
    ReadWritable {
        /// Used to back the creation of code page capability copies
        /// in order to avoid allowing aliased writes to the source code image
        untyped: LocalCap<WUntyped>,
    },
}

/// A virtual address space manager.
pub struct VSpace<State: VSpaceState = vspace_state::Imaged> {
    /// The cap to this address space's root-of-the-tree item.
    root: LocalCap<PagingRoot>,
    /// The id of this address space.
    asid: LocalCap<AssignedASID>,
    /// The recursive structure which represents an address space
    /// structure. `AddressSpace` is a type which is exported by
    /// `crate::arch` and has architecture specific implementations.
    layers: AddressSpace,
    /// When a map request comes in which does not target a specific
    /// address, this helps the VSpace decide where to put that
    /// region.
    next_addr: usize,
    /// The following two members are the resources used by the VSpace
    /// when building out intermediate layers.
    untyped: WUntyped,
    slots: WCNodeSlots,
    _state: PhantomData<State>,
}

impl VSpace<vspace_state::Empty> {
    pub(crate) fn new(
        mut root_cap: LocalCap<PagingRoot>,
        asid: LocalCap<UnassignedASID>,
        slots: WCNodeSlots,
        untyped: WUntyped,
    ) -> Result<Self, VSpaceError> {
        let assigned_asid = asid.assign(&mut root_cap)?;
        Ok(VSpace {
            root: root_cap,
            asid: assigned_asid,
            layers: AddressSpace::new(),
            next_addr: 0,
            untyped,
            slots,
            _state: PhantomData,
        })
    }
}
impl<S: VSpaceState> VSpace<S> {
    /// This address space's id.
    pub(crate) fn asid(&self) -> u32 {
        self.asid.cap_data.asid
    }

    /// Map a given page at some address, I don't care where.
    ///
    /// Note: Generally, we should be operating on regions, but in the
    /// case of the system call for configuring a TCB, a mapped page's
    /// vaddr and its cap must be provided. To obfuscate these behind
    /// a region seems unnecessary. Therefore we provide a crate-only
    /// method to talk about mapping only a page.
    pub(crate) fn map_given_page(
        &mut self,
        page: LocalCap<Page<page_state::Unmapped>>,
        rights: CapRights,
    ) -> Result<LocalCap<Page<page_state::Mapped>>, VSpaceError> {
        match self.layers.map_item(
            &page,
            self.next_addr,
            &mut self.root,
            rights,
            &mut self.untyped,
            &mut self.slots,
        ) {
            Err(MappingError::PageMapFailure(e)) => return Err(VSpaceError::SeL4Error(e)),
            Err(MappingError::IntermediateLayerFailure(e)) => {
                return Err(VSpaceError::SeL4Error(e));
            }
            Err(e) => return Err(VSpaceError::MappingError(e)),
            Ok(_) => (),
        };
        let vaddr = self.next_addr;
        self.next_addr += PageBits::USIZE;
        Ok(Cap {
            cptr: page.cptr,
            cap_data: Page {
                state: page_state::Mapped {
                    asid: self.asid(),
                    vaddr,
                },
            },
            _role: PhantomData,
        })
    }
}

impl VSpace<vspace_state::Imaged> {
    pub fn new(
        paging_root: LocalCap<PagingRoot>,
        asid: LocalCap<UnassignedASID>,
        slots: WCNodeSlots,
        paging_untyped: WUntyped,
        // Things relating to user image code
        code_image_config: ProcessCodeImageConfig,
        user_image: &UserImage<role::Local>,
        _parent_vspace: &mut VSpace, // for temporary mapping for copying
        parent_cnode: &LocalCap<LocalCNode>,
    ) -> Result<Self, VSpaceError> {
        let (code_slots, slots) = match slots.split(user_image.pages_count()) {
            Ok(t) => t,
            Err(_) => return Err(VSpaceError::InsufficientCNodeSlots),
        };
        let mut vspace =
            VSpace::<vspace_state::Empty>::new(paging_root, asid, slots, paging_untyped)?;

        // Map the code image into the process VSpace
        match code_image_config {
            ProcessCodeImageConfig::ReadOnly => {
                for (page_cap, slot) in user_image.pages_iter().zip(code_slots.into_strong_iter()) {
                    let copied_page_cap = page_cap.copy(&parent_cnode, slot, CapRights::R)?;
                    // Use map_page_direct instead of a VSpace so we don't have to keep
                    // track of bulk allocations which cross page table boundaries at
                    // the type level.
                    let _ = vspace.map_given_page(copied_page_cap, CapRights::R)?;
                }
            }
            ProcessCodeImageConfig::ReadWritable { .. } => unimplemented!(),
        }

        Ok(VSpace {
            root: vspace.root,
            asid: vspace.asid,
            layers: vspace.layers,
            next_addr: vspace.next_addr,
            untyped: vspace.untyped,
            slots: vspace.slots,
            _state: PhantomData,
        })
    }

    /// `bootstrap` is used to wrap the root thread's address space.
    pub(crate) fn bootstrap(
        root_vspace_cptr: usize,
        next_addr: usize,
        root_cnode_cptr: usize,
        asid: LocalCap<AssignedASID>,
    ) -> Self {
        VSpace {
            layers: AddressSpace::new(),
            root: Cap {
                cptr: root_vspace_cptr,
                cap_data: PagingRoot::phantom_instance(),
                _role: PhantomData,
            },
            untyped: WUntyped { size: 0 },
            slots: Cap {
                cptr: root_cnode_cptr,
                cap_data: WCNodeSlotsData { offset: 0, size: 0 },
                _role: PhantomData,
            },
            next_addr,
            asid,
            _state: PhantomData,
        }
    }

    /// Map a region of memory at some address, I don't care where.
    pub fn map_region<SizeBits: Unsigned>(
        &mut self,
        region: UnmappedMemoryRegion<SizeBits, shared_status::Exclusive>,
        rights: CapRights,
    ) -> Result<MappedMemoryRegion<SizeBits, shared_status::Exclusive>, VSpaceError>
    where
        SizeBits: IsGreaterOrEqual<PageBits>,
        SizeBits: Sub<PageBits>,
        <SizeBits as Sub<PageBits>>::Output: Unsigned,
        <SizeBits as Sub<PageBits>>::Output: _Pow,
        Pow<<SizeBits as Sub<PageBits>>::Output>: Unsigned,
    {
        self.map_region_internal(region, rights)
    }

    /// Map a _shared_ region of memory at some address, I don't care
    /// where. When `map_shared_region` is called, the caps making up
    /// this region are copied using the slots and cnode provided.
    /// The incoming `UnmappedMemoryRegion` is only borrowed and one
    /// also gets back a new `MappedMemoryRegion` indexed with the
    /// status `Shared`.
    pub fn map_shared_region<SizeBits: Unsigned>(
        &mut self,
        region: &UnmappedMemoryRegion<SizeBits, shared_status::Shared>,
        rights: CapRights,
        slots: LocalCNodeSlots<NumPages<SizeBits>>,
        cnode: &LocalCap<LocalCNode>,
    ) -> Result<MappedMemoryRegion<SizeBits, shared_status::Shared>, VSpaceError>
    where
        SizeBits: IsGreaterOrEqual<PageBits>,
        SizeBits: Sub<PageBits>,
        <SizeBits as Sub<PageBits>>::Output: Unsigned,
        <SizeBits as Sub<PageBits>>::Output: _Pow,
        Pow<<SizeBits as Sub<PageBits>>::Output>: Unsigned,
    {
        let unmapped_sr: UnmappedMemoryRegion<_, shared_status::Shared> = UnmappedMemoryRegion {
            caps: region.caps.copy(cnode, slots, rights)?,
            _size_bits: PhantomData,
            _shared_status: PhantomData,
        };
        self.map_region_internal(unmapped_sr, rights)
    }

    /// For cases when one does not want to continue to duplicate the
    /// region's constituent caps—meaning that there is only one final
    /// address space in which this region will be mapped—that
    /// unmapped region can be consumed and a mapped region is
    /// returned.
    pub fn map_shared_region_and_consume<SizeBits: Unsigned>(
        &mut self,
        region: UnmappedMemoryRegion<SizeBits, shared_status::Shared>,
        rights: CapRights,
    ) -> Result<MappedMemoryRegion<SizeBits, shared_status::Shared>, VSpaceError>
    where
        SizeBits: IsGreaterOrEqual<PageBits>,
        SizeBits: Sub<PageBits>,
        <SizeBits as Sub<PageBits>>::Output: Unsigned,
        <SizeBits as Sub<PageBits>>::Output: _Pow,
        Pow<<SizeBits as Sub<PageBits>>::Output>: Unsigned,
    {
        self.map_region_internal(region, rights)
    }

    // TODO - add more safety rails to prevent returning something from the
    // inner function that becomes invalid when the page is unmapped locally
    /// Map a region temporarily and do with it as thou wilt with `f`.
    ///
    /// Note that this is defined on a region which has the shared
    /// status of `Exclusive`. The idea here is to do the initial
    /// region-filling work with `temporarily_map_region` _before_
    /// sharing this page and mapping it into other address
    /// spaces. This enforced order ought to prevent one from
    /// forgetting to do the region-filling initialization.
    pub(crate) fn temporarily_map_region<SizeBits: Unsigned, F, Out>(
        &mut self,
        region: &mut UnmappedMemoryRegion<SizeBits, shared_status::Exclusive>,
        f: F,
    ) -> Result<Out, VSpaceError>
    where
        SizeBits: IsGreaterOrEqual<PageBits>,
        SizeBits: Sub<PageBits>,
        <SizeBits as Sub<PageBits>>::Output: Unsigned,
        <SizeBits as Sub<PageBits>>::Output: _Pow,
        Pow<<SizeBits as Sub<PageBits>>::Output>: Unsigned,
        F: Fn(&mut MappedMemoryRegion<SizeBits, shared_status::Exclusive>) -> Out,
    {
        let mut mapped_region = self.map_region(
            UnmappedMemoryRegion {
                caps: CapRange::new(region.caps.start_cptr),
                _size_bits: PhantomData,
                _shared_status: PhantomData,
            },
            CapRights::RW,
        )?;
        let res = f(&mut mapped_region);
        let _ = self.unmap_region(mapped_region)?;
        Ok(res)
    }

    /// Unmap a region.
    pub fn unmap_region<SizeBits: Unsigned, SS: SharedStatus>(
        &mut self,
        region: MappedMemoryRegion<SizeBits, SS>,
    ) -> Result<UnmappedMemoryRegion<SizeBits, SS>, VSpaceError>
    where
        SizeBits: IsGreaterOrEqual<PageBits>,
        SizeBits: Sub<PageBits>,
        <SizeBits as Sub<PageBits>>::Output: Unsigned,
        <SizeBits as Sub<PageBits>>::Output: _Pow,
        Pow<<SizeBits as Sub<PageBits>>::Output>: Unsigned,
    {
        let start_cptr = region.caps.initial_cptr;
        for page_cap in region.caps.iter() {
            let _ = self.unmap_page(page_cap)?;
        }
        Ok(UnmappedMemoryRegion {
            caps: CapRange::new(start_cptr),
            _size_bits: PhantomData,
            _shared_status: PhantomData,
        })
    }

    pub(crate) fn root_cptr(&self) -> usize {
        self.root.cptr
    }

    fn unmap_page(
        &mut self,
        page: LocalCap<Page<page_state::Mapped>>,
    ) -> Result<LocalCap<Page<page_state::Unmapped>>, SeL4Error> {
        match unsafe { seL4_ARM_Page_Unmap(page.cptr) } {
            0 => Ok(Cap {
                cptr: page.cptr,
                cap_data: Page {
                    state: page_state::Unmapped {},
                },
                _role: PhantomData,
            }),
            e => Err(SeL4Error::PageUnmap(e)),
        }
    }

    fn map_region_internal<SizeBits: Unsigned, SSIn: SharedStatus, SSOut: SharedStatus>(
        &mut self,
        region: UnmappedMemoryRegion<SizeBits, SSIn>,
        rights: CapRights,
    ) -> Result<MappedMemoryRegion<SizeBits, SSOut>, VSpaceError>
    where
        SizeBits: IsGreaterOrEqual<PageBits>,
        SizeBits: Sub<PageBits>,
        <SizeBits as Sub<PageBits>>::Output: Unsigned,
        <SizeBits as Sub<PageBits>>::Output: _Pow,
        Pow<<SizeBits as Sub<PageBits>>::Output>: Unsigned,
    {
        let vaddr = self.next_addr;
        // create the mapped region first because we need to pluck out
        // the `start_cptr` before the iteration below consumes the
        // unmapped region.
        let mapped_region = MappedMemoryRegion {
            caps: MappedPageRange::new(region.caps.start_cptr, vaddr, self.asid()),
            asid: self.asid(),
            _size_bits: PhantomData,
            _shared_status: PhantomData,
            vaddr,
        };
        for page_cap in region.caps.iter() {
            self.map_given_page(page_cap, rights)?;
        }
        Ok(mapped_region)
    }

    pub(crate) fn skip_pages(&mut self, count: usize) -> Result<(), VSpaceError> {
        if let Some(next) = PageBytes::USIZE
            .checked_mul(count)
            .and_then(|bytes| self.next_addr.checked_add(bytes))
        {
            self.next_addr = next;
            Ok(())
        } else {
            Err(VSpaceError::ExceededAvailableAddressSpace)
        }
    }
}

mod private {
    use super::shared_status::{Exclusive, Shared};
    pub trait SealedSharedStatus {}
    impl SealedSharedStatus for Shared {}
    impl SealedSharedStatus for Exclusive {}

    use super::vspace_state::{Empty, Imaged};
    pub trait SealedVSpaceState {}
    impl SealedVSpaceState for Empty {}
    impl SealedVSpaceState for Imaged {}
}
