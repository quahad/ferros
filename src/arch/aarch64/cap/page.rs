use selfe_sys::*;

use crate::cap::{CapType, CopyAliasable, DirectRetype, PhantomCap};

pub trait PageState: private::SealedPageState {}

pub mod page_state {
    pub struct Mapped {
        pub(super) vaddr: usize,
    }
    impl super::PageState for Mapped {}

    pub struct Unmapped;
    impl super::PageState for Unmapped {}
}

pub struct Page<State: PageState> {
    state: State,
}

impl Page<page_state::Mapped> {
    pub fn vaddr(&self) -> usize {
        self.state.vaddr
    }
}

impl<State: PageState> CapType for Page<State> {}

impl DirectRetype for Page<page_state::Unmapped> {
    type SizeBits = super::super::PageBits;
    fn sel4_type_id() -> usize {
        _object_seL4_ARM_SmallPageObject as usize
    }
}

impl CopyAliasable for Page<page_state::Unmapped> {
    type CopyOutput = Self;
}

impl CopyAliasable for Page<page_state::Mapped> {
    type CopyOutput = Page<page_state::Unmapped>;
}

impl PhantomCap for Page<page_state::Unmapped> {
    fn phantom_instance() -> Self {
        Page {
            state: page_state::Unmapped {},
        }
    }
}

mod private {
    pub trait SealedPageState {}
    impl SealedPageState for super::page_state::Unmapped {}
    impl SealedPageState for super::page_state::Mapped {}
}
