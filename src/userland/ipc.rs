use core::marker::PhantomData;

use selfe_sys::*;

use crate::arch;
use crate::cap::{
    role, Badge, CNode, CNodeRole, CNodeSlot, Cap, DirectRetype, Endpoint, LocalCNode,
    LocalCNodeSlot, LocalCNodeSlots, LocalCap, Notification, Untyped,
};
use crate::error::SeL4Error;
use crate::userland::multi_consumer::WakerSetup;
use crate::userland::shared_memory_ipc::WAKER_BADGE;
use crate::userland::CapRights;
use crate::vspace::VSpaceError;
use typenum::U2;

#[derive(Debug)]
pub enum IPCError {
    RequestSizeTooBig,
    ResponseSizeTooBig,
    ResponseSizeMismatch,
    RequestSizeMismatch,
    SeL4Error(SeL4Error),
    VSpaceError(VSpaceError),
}

impl From<SeL4Error> for IPCError {
    fn from(s: SeL4Error) -> Self {
        IPCError::SeL4Error(s)
    }
}

impl From<VSpaceError> for IPCError {
    fn from(v: VSpaceError) -> Self {
        IPCError::VSpaceError(v)
    }
}

pub struct IpcSetup<'a, Req, Rsp> {
    endpoint: LocalCap<Endpoint>,
    endpoint_cnode: &'a LocalCap<LocalCNode>,
    _req: PhantomData<Req>,
    _rsp: PhantomData<Rsp>,
}

/// Fastpath call channel -> given some memory capacity, a local cnode, and a
/// target responder cnode, create an endpoint locally, copy it to the responder
/// process cnode, and return an IpcSetup to allow connecting callers.
pub fn call_channel<Req: Send + Sync, Rsp: Send + Sync, ResponderRole: CNodeRole>(
    untyped: LocalCap<Untyped<<Endpoint as DirectRetype>::SizeBits>>,
    local_cnode: &LocalCap<LocalCNode>,
    local_slot: LocalCNodeSlot,
    responder_slot: CNodeSlot<ResponderRole>,
) -> Result<(IpcSetup<Req, Rsp>, Responder<Req, Rsp, ResponderRole>), IPCError> {
    let _ = IPCBuffer::<Req, Rsp>::new()?; // Check buffer fits Req and Rsp
    let local_endpoint: LocalCap<Endpoint> = untyped.retype(local_slot)?;
    let responder_endpoint = local_endpoint.copy(&local_cnode, responder_slot, CapRights::RW)?;

    Ok((
        IpcSetup {
            endpoint: local_endpoint,
            endpoint_cnode: &local_cnode,
            _req: PhantomData,
            _rsp: PhantomData,
        },
        Responder {
            endpoint: responder_endpoint,
            _req: PhantomData,
            _rsp: PhantomData,
            _role: PhantomData,
        },
    ))
}

pub fn call_channel_with_waker<Req: Send + Sync, Rsp: Send + Sync, ResponderRole: CNodeRole>(
    untyped: LocalCap<Untyped<<Endpoint as DirectRetype>::SizeBits>>,
    notification_ut: LocalCap<Untyped<<Notification as DirectRetype>::SizeBits>>,
    local_cnode: &LocalCap<LocalCNode>,
    local_slots: LocalCNodeSlots<U2>,
    responder_slot: CNodeSlot<ResponderRole>,
) -> Result<
    (
        IpcSetup<Req, Rsp>,
        Responder<Req, Rsp, ResponderRole>,
        LocalCap<Notification>,
        WakerSetup,
    ),
    IPCError,
> {
    let _ = IPCBuffer::<Req, Rsp>::new()?; // Check buffer fits Req and Rsp
    let (local_slot, local_slots) = local_slots.alloc();
    let local_endpoint: LocalCap<Endpoint> = untyped.retype(local_slot)?;
    let responder_endpoint = local_endpoint.copy(&local_cnode, responder_slot, CapRights::RW)?;

    let (local_slot, _local_slots) = local_slots.alloc();
    let notification: LocalCap<Notification> = notification_ut.retype(local_slot)?;

    Ok((
        IpcSetup {
            endpoint: local_endpoint,
            endpoint_cnode: &local_cnode,
            _req: PhantomData,
            _rsp: PhantomData,
        },
        Responder {
            endpoint: responder_endpoint,
            _req: PhantomData,
            _rsp: PhantomData,
            _role: PhantomData,
        },
        Cap::wrap_cptr(notification.cptr),
        WakerSetup {
            interrupt_badge: Badge::from(WAKER_BADGE),
            notification,
        },
    ))
}

impl<'a, Req, Rsp> IpcSetup<'a, Req, Rsp> {
    pub fn create_caller<Role: CNodeRole>(
        &self,
        caller_slot: CNodeSlot<Role>,
    ) -> Result<Caller<Req, Rsp, Role>, IPCError> {
        let caller_endpoint =
            self.endpoint
                .copy(&self.endpoint_cnode, caller_slot, CapRights::RWG)?;

        Ok(Caller {
            endpoint: caller_endpoint,
            _req: PhantomData,
            _rsp: PhantomData,
        })
    }
}

#[derive(Debug)]
pub struct Caller<Req: Sized, Rsp: Sized, Role: CNodeRole> {
    endpoint: Cap<Endpoint, Role>,
    _req: PhantomData<Req>,
    _rsp: PhantomData<Rsp>,
}

/// Internal convenience for working with IPC Buffer instances
/// *Note:* In a given thread or process, all instances of
/// IPCBuffer wrap a pointer to the very same underlying buffer.
pub(crate) struct IPCBuffer<'a, Req: Sized, Rsp: Sized> {
    buffer: &'a mut seL4_IPCBuffer,
    _req: PhantomData<Req>,
    _rsp: PhantomData<Rsp>,
}

impl<'a, Req: Sized, Rsp: Sized> IPCBuffer<'a, Req, Rsp> {
    /// Don't forget that while this says `new` in the signature,
    /// it is still aliasing the thread-global IPC Buffer pointer
    pub(crate) fn new() -> Result<Self, IPCError> {
        let request_size = core::mem::size_of::<Req>();
        let response_size = core::mem::size_of::<Rsp>();
        let buffer = unchecked_raw_ipc_buffer();
        let buffer_size = core::mem::size_of_val(&buffer.msg);
        // TODO - Move this to compile-time somehow
        if request_size > buffer_size {
            return Err(IPCError::RequestSizeTooBig);
        }
        if response_size > buffer_size {
            return Err(IPCError::ResponseSizeTooBig);
        }
        Ok(IPCBuffer {
            buffer,
            _req: PhantomData,
            _rsp: PhantomData,
        })
    }

    /// Maximum size of IPC Buffer message contents, in bytes
    pub(crate) fn max_size() -> usize {
        let buffer = unchecked_raw_ipc_buffer();
        core::mem::size_of_val(&buffer.msg)
    }

    /// Don't forget that while this says `new` in the signature,
    /// it is still aliasing the thread-global IPC Buffer pointer
    ///
    /// Use only when all possible prior paths have conclusively
    /// checked sizing constraints
    pub(crate) unsafe fn unchecked_new() -> Self {
        IPCBuffer {
            buffer: unchecked_raw_ipc_buffer(),
            _req: PhantomData,
            _rsp: PhantomData,
        }
    }

    unsafe fn unchecked_copy_into_buffer<T: Sized>(&mut self, data: &T) {
        core::ptr::copy(
            data as *const T,
            &self.buffer.msg as *const [usize] as *const T as *mut T,
            1,
        );
    }
    unsafe fn unchecked_copy_from_buffer<T: Sized>(&self) -> T {
        let mut data = core::mem::zeroed();
        core::ptr::copy_nonoverlapping(
            &self.buffer.msg as *const [usize] as *const T,
            &mut data as *mut T,
            1,
        );
        data
    }

    pub fn copy_req_into_buffer(&mut self, request: &Req) {
        unsafe { self.unchecked_copy_into_buffer(request) }
    }

    pub fn copy_req_from_buffer(&self) -> Req {
        unsafe { self.unchecked_copy_from_buffer() }
    }

    fn copy_rsp_into_buffer(&mut self, response: &Rsp) {
        unsafe { self.unchecked_copy_into_buffer(response) }
    }
    fn copy_rsp_from_buffer(&mut self) -> Rsp {
        unsafe { self.unchecked_copy_from_buffer() }
    }
}

#[inline]
fn unchecked_raw_ipc_buffer<'a>() -> &'a mut seL4_IPCBuffer {
    unsafe { &mut *seL4_GetIPCBuffer() }
}

pub(crate) fn type_length_in_words<T>() -> usize {
    let t_bytes = core::mem::size_of::<T>();
    let usize_bytes = core::mem::size_of::<usize>();
    if t_bytes == 0 {
        return 0;
    }
    if t_bytes < usize_bytes {
        return 1;
    }
    let words = t_bytes / usize_bytes;
    let rem = t_bytes % usize_bytes;
    if rem > 0 {
        words + 1
    } else {
        words
    }
}

fn type_length_message_info<T>() -> seL4_MessageInfo_t {
    unsafe {
        seL4_MessageInfo_new(
            0,                                               // label,
            0,                                               // capsUnwrapped,
            0,                                               // extraCaps,
            arch::to_sel4_word(type_length_in_words::<T>()), // length in words!
        )
    }
}

pub struct MessageInfo {
    inner: seL4_MessageInfo_t,
}

impl MessageInfo {
    pub fn label(&self) -> usize {
        unsafe {
            seL4_MessageInfo_ptr_get_label(
                &self.inner as *const seL4_MessageInfo_t as *mut seL4_MessageInfo_t,
            ) as usize
        }
    }

    /// Length of the message in words, ought to be
    /// less than the length of the IPC Buffer's msg array,
    /// an array of `usize` words.
    pub(crate) fn length_words(&self) -> usize {
        unsafe {
            seL4_MessageInfo_ptr_get_length(
                &self.inner as *const seL4_MessageInfo_t as *mut seL4_MessageInfo_t,
            ) as usize
        }
    }

    /// Does this message info have the label tag
    /// that indicates that no fault has occurred?
    pub(crate) fn has_null_fault_label(&self) -> bool {
        const NULL_FAULT: usize = seL4_Fault_tag_seL4_Fault_NullFault as usize;
        self.label() == NULL_FAULT
    }
}

impl From<seL4_MessageInfo_t> for MessageInfo {
    fn from(msg: seL4_MessageInfo_t) -> Self {
        MessageInfo { inner: msg }
    }
}

impl<Req, Rsp> Caller<Req, Rsp, role::Child> {
    pub fn as_cap(self) -> Cap<Endpoint, role::Child> {
        self.endpoint
    }
}

impl<Req, Rsp> Caller<Req, Rsp, role::Local> {
    pub fn wrap_cptr(cptr: usize) -> Caller<Req, Rsp, role::Local> {
        Caller {
            endpoint: Cap::wrap_cptr(cptr),
            _req: PhantomData,
            _rsp: PhantomData,
        }
    }
}

impl<Req, Rsp> Caller<Req, Rsp, role::Local> {
    pub fn blocking_call<'a>(&self, request: &Req) -> Result<Rsp, IPCError> {
        // Can safely use unchecked_new because we check sizing during the creation of Caller
        let mut ipc_buffer = unsafe { IPCBuffer::unchecked_new() };
        let msg_info: MessageInfo = unsafe {
            ipc_buffer.copy_req_into_buffer(request);
            seL4_Call(self.endpoint.cptr, type_length_message_info::<Req>())
        }
        .into();
        if msg_info.length_words() != type_length_in_words::<Rsp>() {
            return Err(IPCError::ResponseSizeMismatch);
        }
        Ok(ipc_buffer.copy_rsp_from_buffer())
    }
}

#[derive(Debug)]
pub struct Responder<Req: Sized, Rsp: Sized, Role: CNodeRole> {
    endpoint: Cap<Endpoint, Role>,
    _req: PhantomData<Req>,
    _rsp: PhantomData<Rsp>,
    _role: PhantomData<Role>,
}

impl<Req, Rsp> Responder<Req, Rsp, role::Child> {
    pub fn as_cap(self) -> Cap<Endpoint, role::Child> {
        self.endpoint
    }
}

impl<Req, Rsp> Responder<Req, Rsp, role::Local> {
    pub fn wrap_cptr(cptr: usize) -> Responder<Req, Rsp, role::Local> {
        Responder {
            endpoint: Cap::wrap_cptr(cptr),
            _req: PhantomData,
            _rsp: PhantomData,
            _role: PhantomData,
        }
    }

    pub fn reply_recv<F>(self, mut f: F) -> Result<Rsp, IPCError>
    where
        F: FnMut(Req) -> (Rsp),
    {
        self.reply_recv_with_state((), move |req, state| (f(req), state))
    }

    pub fn reply_recv_with_state<F, State>(
        self,
        initial_state: State,
        f: F,
    ) -> Result<Rsp, IPCError>
    where
        F: FnMut(Req, State) -> (Rsp, State),
    {
        self.reply_recv_with_notification(initial_state, f, move |_sender_badge, state| state)
    }

    pub fn reply_recv_with_notification<F, G, State>(
        self,
        initial_state: State,
        mut f: F,
        mut g: G,
    ) -> Result<Rsp, IPCError>
    where
        F: FnMut(Req, State) -> (Rsp, State),
        G: FnMut(usize, State) -> State,
    {
        // Can safely use unchecked_new because we check sizing during the creation of Responder
        let mut ipc_buffer = unsafe { IPCBuffer::unchecked_new() };
        let mut sender_badge: usize = 0;
        // Do a regular receive to seed our initial value
        let mut msg_info: MessageInfo =
            unsafe { seL4_Recv(self.endpoint.cptr, &mut sender_badge as *mut usize) }.into();

        let request_length_in_words = type_length_in_words::<Req>();
        let mut response;
        let mut state = initial_state;
        loop {
            // if the badge is zero, it's a regular IPC
            if sender_badge == 0 {
                if msg_info.length_words() != request_length_in_words {
                    // A wrong-sized message length is an indication of unforeseen or
                    // misunderstood kernel operations. Using the checks established in
                    // the creation of Caller/Responder sets should prevent the creation
                    // of wrong-sized messages through their expected paths.
                    //
                    // Not knowing what this incoming message is, we drop it and spin-fail the loop.
                    // Note that `continue`'ing from here will cause this process
                    // to loop forever doing this check with no fresh data, most likely leaving the
                    // caller perpetually blocked.
                    debug_println!("Request size incoming ({} words) does not match static size expectation ({} words).",
                msg_info.length_words(), request_length_in_words);
                    continue;
                }
                let out = f(ipc_buffer.copy_req_from_buffer(), state);
                response = out.0;
                state = out.1;

                ipc_buffer.copy_rsp_into_buffer(&response);
                msg_info = unsafe {
                    seL4_ReplyRecv(
                        self.endpoint.cptr,
                        type_length_message_info::<Rsp>(),
                        &mut sender_badge as *mut usize,
                    )
                }
                .into();
            } else {
                // nonzero badges are from a notification
                state = g(sender_badge, state);

                msg_info =
                    unsafe { seL4_Recv(self.endpoint.cptr, &mut sender_badge as *mut usize) }
                        .into();
            }
        }
    }

    pub fn recv_reply_once<F>(&self, mut f: F) -> Result<(), IPCError>
    where
        F: FnMut(Req) -> (Rsp),
    {
        // Can safely use unchecked_new because we check sizing during the creation of Responder
        let mut ipc_buffer = unsafe { IPCBuffer::unchecked_new() };
        let mut sender_badge: usize = 0;
        // Do a regular receive to seed our initial value
        let msg_info: MessageInfo =
            unsafe { seL4_Recv(self.endpoint.cptr, &mut sender_badge as *mut usize) }.into();

        let request_length_in_words = type_length_in_words::<Req>();
        if msg_info.length_words() != request_length_in_words {
            // A wrong-sized message length is an indication of unforeseen or
            // misunderstood kernel operations. Using the checks established in
            // the creation of Caller/Responder sets should prevent the creation
            // of wrong-sized messages through their expected paths.
            //
            // Not knowing what this incoming message is, we drop it and spin-fail the loop.
            // Note that `continue`'ing from here will cause this process
            // to loop forever doing this check with no fresh data, most likely leaving the caller perpetually blocked.
            debug_println!("Request size incoming ({} words) does not match static size expectation ({} words).",
                msg_info.length_words(), request_length_in_words);
            return Err(IPCError::RequestSizeMismatch);
        }

        let response = f(ipc_buffer.copy_req_from_buffer());
        ipc_buffer.copy_rsp_into_buffer(&response);

        unsafe {
            seL4_Reply(type_length_message_info::<Rsp>());
        }

        Ok(())
    }
}

#[derive(Debug)]
pub struct Sender<Msg: Sized, Role: CNodeRole> {
    pub(crate) endpoint: Cap<Endpoint, Role>,
    pub(crate) _msg: PhantomData<Msg>,
}

impl<Msg: Sized> Sender<Msg, role::Local> {
    pub fn blocking_send<'a>(&self, message: &Msg) -> Result<(), IPCError> {
        // Using unchecked_new is acceptable here because we check the message size
        // constraints during the construction of Sender + FaultOrMessageHandler
        let mut ipc_buffer: IPCBuffer<Msg, ()> = unsafe { IPCBuffer::unchecked_new() };
        ipc_buffer.copy_req_into_buffer(message);
        unsafe {
            seL4_Send(self.endpoint.cptr, type_length_message_info::<Msg>());
        }
        Ok(())
    }
}

impl<Msg: Sized, Role: CNodeRole> Sender<Msg, Role> {
    pub fn copy<DestRole: CNodeRole>(
        &self,
        cnode: &LocalCap<CNode<Role>>,
        dest_slot: CNodeSlot<DestRole>,
    ) -> Result<Sender<Msg, DestRole>, SeL4Error> {
        Ok(Sender {
            endpoint: self.endpoint.copy(cnode, dest_slot, CapRights::RWG)?,
            _msg: PhantomData,
        })
    }
}
