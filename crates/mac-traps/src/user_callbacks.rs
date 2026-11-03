//! One core-only user callback executor shared by the std and no_std CRTs.
//! Each real runtime supplies its existing allocator-backed initialized storage
//! and failure policy. No resident implementation or host callback runs here.
use mac_abi::{definitions::*, *};

/// Owned initialized user storage. A borrow stays stable until the next grow;
/// grow must initialize every returned word before reporting success.
pub trait Storage
{
    fn words(&mut self) -> &mut [u64];
    fn grow(&mut self, words: usize) -> Result<(), Status>;
}
pub type Failure = fn(Status, &'static [u8]) -> !;

unsafe extern "C" fn gate(number: u32, pointer: *mut (), bytes: usize) -> i64
{
    let mut result = pointer.addr();
    // SAFETY: each caller lends its initialized local wire record only for this
    // synchronous trap. System snapshots all user records and nested buffers.
    unsafe {
        core::arch::asm!("ecall", in("a7") number,
                        inlateout("a0") result, in("a1") bytes, clobber_abi("C"));
    }
    result as i64
}

fn perform(call: &mut CallbackCall) -> Result<(), Status>
{
    let result = unsafe {
        gate(mac_abi::traps::USER_CALLBACK,
             core::ptr::from_mut(call).cast(),
             size_of::<CallbackCall>())
    };
    if result == 0 { Ok(()) } else { Err(Status::try_from(result).unwrap_or(BAD_ABI)) }
}

/// Enter only on the genuine callback THRD. Empty NEXT remains parked in
/// System; this loop never polls or invents an execution identity.
pub fn run(storage: &mut impl Storage, failure: Failure) -> !
{
    loop
    {
        let words = storage.words();
        let Some(bytes) = words.len().checked_mul(size_of::<u64>())
        else
        {
            failure(NO_MEMORY, b"callback storage extent overflow");
        };
        let pointer = words.as_mut_ptr().cast();
        let mut call = CallbackCall { buffer: pointer,
                                      length: bytes,
                                      ..Default::default() };
        if let Err(status) = perform(&mut call)
        {
            failure(status, b"callback work delivery refused");
        }
        if call.present == 0 && call.required > bytes
        {
            let Some(words) = call.required.checked_add(7).map(|n| n / 8)
            else
            {
                failure(NO_MEMORY, b"callback packet extent overflow");
            };
            if let Err(status) = storage.grow(words)
            {
                failure(status, b"callback packet allocation refused");
            }
            if storage.words().len() < words
            {
                failure(BAD_ABI, b"callback storage was not initialized");
            }
            continue;
        }
        if call.present != 1 || call.required > bytes || call.required < size_of::<CallbackPacket>()
        {
            failure(BAD_ABI, b"callback delivery did not contain admitted work");
        }
        let result = unsafe { invoke(pointer, call.required) }.unwrap_or_else(i64::from);
        let mut returned = CallbackCall { operation: CALLBACK_RETURN,
                                          buffer: pointer,
                                          length: call.required,
                                          result,
                                          ..Default::default() };
        if let Err(status) = perform(&mut returned)
        {
            failure(status, b"callback result delivery refused");
        }
    }
}

/// # Safety
/// `pointer` borrows initialized, exclusive, eight-byte-aligned packet storage
/// of exactly `length` bytes until the author call returns. Every author pointer
/// for copied families remains within that same user allocation or a local
/// stack record. Opaque work instead carries the same Process's original U addresses;
/// this executor does not dereference them. Malformed copied-family metadata is
/// rejected before making borrows.
unsafe fn invoke(pointer: *mut u8, length: usize) -> Result<i64, Status>
{
    if pointer.is_null()
       || !pointer.addr().is_multiple_of(8)
       || length < size_of::<CallbackPacket>()
       || length > isize::MAX as usize
    {
        return Err(BAD_ABI);
    }
    let packet = unsafe { pointer.cast::<CallbackPacket>().read() };
    if !packet.header.supports::<CallbackPacket>()
       || packet.reserved != 0
       || packet.reserved2 != 0
       || packet.flags & !CALLBACK_SURFACE != 0
       || packet.entry == 0
       || !packet.entry.is_multiple_of(2)
    {
        return Err(BAD_ABI);
    }
    if packet.argument_kind == CALLBACK_OPAQUE
    {
        if packet.flags != 0
           || packet.object.length != 0
           || packet.arguments.length != 0
           || packet.auxiliary.iter().any(|r| r.offset != 0 || r.length != 0)
           || packet.pixels.offset != 0
           || packet.pixels.length != 0
           || length != size_of::<CallbackPacket>()
        {
            return Err(BAD_ABI);
        }
        let mut call = DefinitionCall { header: Header::for_type::<DefinitionCall>(),
                                        kind: packet.kind,
                                        id: packet.id,
                                        variation: packet.variation,
                                        message: packet.message,
                                        reserved: 0,
                                        param: packet.param,
                                        object: packet.object.offset as usize as *mut (),
                                        surface: packet.arguments.offset as usize
                                                 as *mut Surface,
                                        gate,
                                        reflow: packet.reflow };
        // Only the same genuine Process receives this route. System never
        // follows these original U addresses; invalid access faults that app.
        let entry: DefinitionEntry = unsafe { core::mem::transmute(packet.entry as usize) };
        return Ok(unsafe { entry(&mut call) });
    }
    let ranges =
        [packet.object, packet.arguments, packet.auxiliary[0], packet.auxiliary[1], packet.pixels];
    let mut checked = [0..0, 0..0, 0..0, 0..0, 0..0];
    for (i, range) in ranges.into_iter().enumerate()
    {
        let range = range.checked(length, 8).ok_or(BAD_ABI)?;
        if !range.is_empty() && range.start < size_of::<CallbackPacket>()
        {
            return Err(BAD_ABI);
        }
        for earlier in &checked[..i]
        {
            if range.start < earlier.end && earlier.start < range.end
            {
                return Err(BAD_ABI);
            }
        }
        checked[i] = range;
    }
    if checked[0].is_empty()
    {
        return Err(BAD_ABI);
    }
    let param = match packet.argument_kind
    {
        CALLBACK_SCALAR if checked[1..4].iter().all(|r| r.is_empty()) => packet.param,
        CALLBACK_BYTES if !checked[1].is_empty() && checked[2..4].iter().all(|r| r.is_empty()) =>
        {
            unsafe { pointer.add(checked[1].start) }.addr() as i64
        }
        CALLBACK_GRID_AXIS
            if checked[1].len() == size_of::<controls::reflow::GridAxisCall>()
               && checked[2].len().is_multiple_of(size_of::<controls::reflow::GridTrack>())
               && checked[3].len().is_multiple_of(size_of::<controls::reflow::GridItem>()) =>
        {
            let axis = unsafe {
                &mut *pointer.add(checked[1].start).cast::<controls::reflow::GridAxisCall>()
            };
            axis.tracks = unsafe { pointer.add(checked[2].start) }.cast();
            axis.track_count = checked[2].len() / size_of::<controls::reflow::GridTrack>();
            axis.items = unsafe { pointer.add(checked[3].start) }.cast();
            axis.item_count = checked[3].len() / size_of::<controls::reflow::GridItem>();
            core::ptr::from_mut(axis).addr() as i64
        }
        CALLBACK_GRID_PAINT
            if checked[1].len() == size_of::<controls::reflow::GridPaint>()
               && checked[2].len().is_multiple_of(size_of::<geometry::Rect>())
               && checked[3].is_empty() =>
        {
            let grid = unsafe {
                &mut *pointer.add(checked[1].start).cast::<controls::reflow::GridPaint>()
            };
            grid.cells = unsafe { pointer.add(checked[2].start) }.cast();
            grid.count = checked[2].len() / size_of::<geometry::Rect>();
            core::ptr::from_mut(grid).addr() as i64
        }
        _ => return Err(BAD_ABI)
    };
    let mut surface = Surface { pixels: core::ptr::null_mut(),
                                width:  packet.surface_width,
                                height: packet.surface_height,
                                origin: packet.surface_origin,
                                clip:   packet.surface_clip };
    let surface_pointer = if packet.flags & CALLBACK_SURFACE != 0
    {
        let pixels =
            usize::try_from(surface.width).ok()
                                          .and_then(|w| w.checked_mul(surface.height as usize))
                                          .and_then(|n| n.checked_mul(size_of::<u32>()))
                                          .ok_or(BAD_ABI)?;
        if pixels == 0
           || pixels != checked[4].len()
           || surface.width > i16::MAX as u32
           || surface.height > i16::MAX as u32
           || surface.clip.left < 0
           || surface.clip.top < 0
           || i32::from(surface.clip.right) > surface.width as i32
           || i32::from(surface.clip.bottom) > surface.height as i32
        {
            return Err(BAD_ABI);
        }
        surface.pixels = unsafe { pointer.add(checked[4].start) }.cast();
        core::ptr::from_mut(&mut surface)
    }
    else if checked[4].is_empty()
    {
        core::ptr::null_mut()
    }
    else
    {
        return Err(BAD_ABI);
    };
    let mut call = DefinitionCall { header: Header::for_type::<DefinitionCall>(),
                                    kind: packet.kind,
                                    id: packet.id,
                                    variation: packet.variation,
                                    message: packet.message,
                                    reserved: 0,
                                    param,
                                    object: unsafe { pointer.add(checked[0].start) }.cast(),
                                    surface: surface_pointer,
                                    gate,
                                    reflow: packet.reflow };
    // SAFETY: System admitted entry in this Process's executable mapping. This
    // thread remains U-mode; arbitrary author instructions retain only its own
    // Process authority. All ABI buffers above are actual initialized user data.
    let entry: DefinitionEntry = unsafe { core::mem::transmute(packet.entry as usize) };
    Ok(unsafe { entry(&mut call) })
}
