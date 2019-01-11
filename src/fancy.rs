use core::marker::PhantomData;
use core::ops::{Add, Sub};
use sel4_sys::{
    api_object_seL4_CapTableObject, api_object_seL4_TCBObject, api_object_seL4_UntypedObject,
    seL4_BootInfo, seL4_CPtr, seL4_CapInitThreadCNode, seL4_UntypedDesc, seL4_Untyped_Retype,
    seL4_Word, seL4_WordBits,
};
use typenum::operator_aliases::{Add1, Diff, Shleft, Sub1};
use typenum::{
    Bit, Exp, IsGreaterOrEqual, UInt, UTerm, Unsigned, B0, B1, U1, U19, U2, U24, U256, U3, U32, U5,
    U8,
};

use crate::pow::{Pow, _Pow};

pub trait CapType {
    fn sel4_type_id() -> usize;
}

#[derive(Debug)]
pub struct Capability<CT: CapType> {
    cptr: usize,
    _cap_type: PhantomData<CT>,
}

pub trait CNodeRole {}
pub mod CNodeRoles {
    use super::CNodeRole;

    #[derive(Debug)]
    pub struct CSpaceRoot {}
    impl CNodeRole for CSpaceRoot {}

    #[derive(Debug)]
    pub struct ChildProcess {}
    impl CNodeRole for ChildProcess {}
}

#[derive(Debug)]
pub struct CNode<Radix: Unsigned, FreeSlots: Unsigned, Role: CNodeRole> {
    _radix: PhantomData<Radix>,
    _free_slots: PhantomData<FreeSlots>,
    _role: PhantomData<Role>,
}

impl<Radix: Unsigned, FreeSlots: Unsigned, Role: CNodeRole> CapType
    for CNode<Radix, FreeSlots, Role>
{
    fn sel4_type_id() -> usize {
        api_object_seL4_CapTableObject as usize
    }
}

#[derive(Debug)]
struct CNodeSlot {
    cptr: usize,
    offset: usize,
}

impl<Radix: Unsigned, FreeSlots: Unsigned, Role: CNodeRole>
    Capability<CNode<Radix, FreeSlots, Role>>
where
    FreeSlots: Sub<B1>,
    Sub1<FreeSlots>: Unsigned,
{
    fn consume_slot(self) -> (Capability<CNode<Radix, Sub1<FreeSlots>, Role>>, CNodeSlot) {
        (
            Capability {
                cptr: self.cptr,
                _cap_type: PhantomData,
            },
            CNodeSlot {
                cptr: self.cptr,
                offset: (1 << Radix::to_u8()) - FreeSlots::to_usize(),
            },
        )
    }
}

// TODO: how many slots are there really? We should be able to know this at build
// time.
// Answer: The radix is 19, and there are 12 initial caps.
// TODO: ideally, this should only be callable once in the process. Is that possible?
pub fn root_cnode(
    bootinfo: &'static seL4_BootInfo,
) -> Capability<CNode<U19, U256, CNodeRoles::CSpaceRoot>> {
    Capability {
        cptr: seL4_CapInitThreadCNode as usize,
        _cap_type: PhantomData
        // index: seL4_WordBits as usize,
        // depth: 0,
        // offset: bootinfo.empty.start as usize,
        // _radix: PhantomData,
        // _free_slots: PhantomData,
    }
}

/////////////
// Untyped //
/////////////

#[derive(Debug)]
pub struct Untyped<BitSize: Unsigned> {
    _bit_size: PhantomData<BitSize>,
}

impl<BitSize: Unsigned> CapType for Untyped<BitSize> {
    fn sel4_type_id() -> usize {
        api_object_seL4_UntypedObject as usize
    }
}

pub fn wrap_untyped<BitSize: Unsigned>(
    cptr: usize,
    untyped_desc: &seL4_UntypedDesc,
) -> Option<Capability<Untyped<BitSize>>> {
    if untyped_desc.sizeBits == BitSize::to_u8() {
        Some(Capability {
            cptr,
            _cap_type: PhantomData,
        })
    } else {
        None
    }
}

#[derive(Debug)]
pub enum Error {
    UntypedRetype(u32),
}

pub trait Split<Radix: Unsigned, FreeSlots: Unsigned>
where
    FreeSlots: Sub<B1>,
    Sub1<FreeSlots>: Unsigned,
    <Self as Split<Radix, FreeSlots>>::OutputBitSize: Unsigned,
{
    type OutputBitSize;

    fn split(
        self,
        dest_cnode: Capability<CNode<Radix, FreeSlots, CNodeRoles::CSpaceRoot>>,
    ) -> Result<
        (
            Capability<Untyped<Self::OutputBitSize>>,
            Capability<Untyped<Self::OutputBitSize>>,
            Capability<CNode<Radix, Sub1<FreeSlots>, CNodeRoles::CSpaceRoot>>,
        ),
        Error,
    >;
}

impl<Radix: Unsigned, FreeSlots: Unsigned, BitSize: Unsigned> Split<Radix, FreeSlots>
    for Capability<Untyped<BitSize>>
where
    FreeSlots: Sub<B1>,
    Sub1<FreeSlots>: Unsigned,

    BitSize: Sub<B1>,
    Sub1<BitSize>: Unsigned,
{
    type OutputBitSize = Sub1<BitSize>;

    fn split(
        self,
        dest_cnode: Capability<CNode<Radix, FreeSlots, CNodeRoles::CSpaceRoot>>,
    ) -> Result<
        (
            Capability<Untyped<Self::OutputBitSize>>,
            Capability<Untyped<Self::OutputBitSize>>,
            Capability<CNode<Radix, Sub1<FreeSlots>, CNodeRoles::CSpaceRoot>>,
        ),
        Error,
    > {
        let (dest_cnode, dest_slot) = dest_cnode.consume_slot();

        let err = unsafe {
            seL4_Untyped_Retype(
                self.cptr as u32,                          // _service
                Untyped::<BitSize>::sel4_type_id() as u32, // type
                Self::OutputBitSize::to_u32(),             // size_bits
                dest_slot.cptr as u32,                     // root
                0,                                         // index
                0,                                         // depth
                dest_slot.offset as u32,                   // offset
                1,                                         // num_objects
            )
        };
        if err != 0 {
            return Err(Error::UntypedRetype(err));
        }

        Ok((
            Capability {
                cptr: self.cptr,
                _cap_type: PhantomData,
            },
            Capability {
                cptr: dest_slot.offset,
                _cap_type: PhantomData,
            },
            dest_cnode,
        ))
    }
}

pub trait RetypeLocal<Radix: Unsigned, FreeSlots: Unsigned, TargetCapType: CapType>
where
    FreeSlots: Sub<B1>,
    Sub1<FreeSlots>: Unsigned,
{
    fn retype_local(
        self,
        dest_cnode: Capability<CNode<Radix, FreeSlots, CNodeRoles::CSpaceRoot>>,
    ) -> Result<
        (
            Capability<TargetCapType>,
            Capability<CNode<Radix, Sub1<FreeSlots>, CNodeRoles::CSpaceRoot>>,
        ),
        Error,
    >;
}

impl<Radix: Unsigned, FreeSlots: Unsigned, TargetCapType: CapType, BitSize: Unsigned>
    RetypeLocal<Radix, FreeSlots, TargetCapType> for Capability<Untyped<BitSize>>
where
    FreeSlots: Sub<B1>,
    Sub1<FreeSlots>: Unsigned,
    // TODO: make sure the untyped has enough room for the target object
{
    fn retype_local(
        self,
        dest_cnode: Capability<CNode<Radix, FreeSlots, CNodeRoles::CSpaceRoot>>,
    ) -> Result<
        (
            Capability<TargetCapType>,
            Capability<CNode<Radix, Sub1<FreeSlots>, CNodeRoles::CSpaceRoot>>,
        ),
        Error,
    > {
        let (dest_cnode, dest_slot) = dest_cnode.consume_slot();

        let err = unsafe {
            seL4_Untyped_Retype(
                self.cptr as u32,                     // _service
                TargetCapType::sel4_type_id() as u32, // type
                0,                                    // size_bits
                dest_slot.cptr as u32,                // root
                0,                                    // index
                0,                                    // depth
                dest_slot.offset as u32,              // offset
                1,                                    // num_objects
            )
        };

        if err != 0 {
            return Err(Error::UntypedRetype(err));
        }

        Ok((
            Capability {
                cptr: dest_slot.offset,
                _cap_type: PhantomData,
            },
            dest_cnode,
        ))
    }
}

impl<BitSize: Unsigned> Capability<Untyped<BitSize>> {
    pub fn retype_local_cnode<Radix: Unsigned, FreeSlots: Unsigned, ChildRadix: Unsigned>(
        self,
        dest_cnode: Capability<CNode<Radix, FreeSlots, CNodeRoles::CSpaceRoot>>,
    ) -> Result<
        (
            Capability<CNode<ChildRadix, Pow<ChildRadix>, CNodeRoles::ChildProcess>>,
            Capability<CNode<Radix, Sub1<FreeSlots>, CNodeRoles::CSpaceRoot>>,
        ),
        Error,
    >
    where
        FreeSlots: Sub<B1>,
        Sub1<FreeSlots>: Unsigned,
        ChildRadix: _Pow,
        Pow<ChildRadix>: Unsigned,
    {
        let (dest_cnode, dest_slot) = dest_cnode.consume_slot();

        let err = unsafe {
            seL4_Untyped_Retype(
                self.cptr as u32, // _service
                api_object_seL4_CapTableObject, // type
                ChildRadix::to_u32(), // size_bits
                dest_slot.cptr as u32, // root
                0,                // index
                0,                // depth
                dest_slot.offset as u32, // offset
                1,                // num_objects
            )
        };

        if err != 0 {
            return Err(Error::UntypedRetype(err));
        }

        Ok((
            Capability {
                cptr: dest_slot.offset,
                _cap_type: PhantomData,
            },
            dest_cnode,
        ))
    }
}

/////////
// TCB //
/////////
#[derive(Debug)]
pub struct ThreadControlBlock {}

impl CapType for ThreadControlBlock {
    fn sel4_type_id() -> usize {
        api_object_seL4_TCBObject as usize
    }
}

// trait Configure {
//     fn configure(
//         &self,
//         fault_ep: Capability<Endpoint>,
//         cspace_root: Capability<CNode>,
//         cspace_root_data: usize,
//         vspace_root: Capability<VSpace>,
//         vspace_root_data: usize,
//         buffer: usize,
//         buffer_frame: Capability<Frame>,
//     ) {
//         unimplemented!()
//     }
// }

// Others

// #[derive(Debug)]
// pub struct Endpoint {}

// impl CapType for Endpoint {
//     fn sel4_type_id() -> usize {
//         api_object_seL4_Endpoint as usize
//     }
// }

// #[derive(Debug)]
// pub struct CNode {}

// impl CapType for Endpoint {
//     fn sel4_type_id() -> usize {
//         api_object_seL4_CNode as usize
//     }
// }
