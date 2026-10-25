//! Host ABI borrowing for the real owner-scoped offscreen services.
use mac_abi::{quickdraw::*, traps::*, *};

/// # Safety
/// The service gate's caller lends valid, exclusive request/output storage and
/// readable nested input buffers for this synchronous invocation.
pub(super) unsafe fn dispatch(number: u32, pointer: *mut (), size: usize) -> Option<i64>
{
    let result = (|| -> Result<Option<()>, Status> {
        let target = match number
        {
            QUICKDRAW => unsafe { super::request::<GraphicsCall>(pointer, size) }?.target,
            FONT => unsafe { super::request::<FontCall>(pointer, size) }?.target,
            COPY_BITS => unsafe { super::request::<BitmapCall>(pointer, size) }?.target,
            WORLD => Target::default(),
            _ => return Ok(None)
        };
        let target = target.decode().map_err(|_| BAD_ARGUMENT)?;
        if number != WORLD && !matches!(target, DrawingTarget::World(_))
        {
            return Ok(None);
        }
        let ledger = super::inspection::caller_ledger(super::files::current_owner())?;
        super::SESSION.with(|slot| {
                          let mut session = slot.try_borrow_mut().map_err(|_| SERVICE_BUSY)?;
                          let session = session.as_mut().ok_or(NOT_SUPPORTED)?;
                          let owner = ledger.owner().incarnation();
                          if number == WORLD
                          {
                              let call = unsafe { super::request::<WorldCall>(pointer, size) }?;
                              if call.operation == WORLD_COPY
                              {
                                  if call.reserved != 0
                                  {
                                      return Err(BAD_ARGUMENT);
                                  }
                                  let DrawingTarget::World(destination) =
                                      call.target.decode().map_err(|_| BAD_ARGUMENT)?
                                  else
                                  {
                                      return Err(NOT_SUPPORTED);
                                  };
                                  let (bitmap, data) =
                                      session.worlds.get(call.world, owner)?.copy_request(call)?;
                                  session.worlds
                                         .get_mut(destination.0, owner)?
                                         .bitmap(&bitmap, &data, &[], &[])?;
                              }
                              else
                              {
                                  let pixels = if call.pixel_length == 0
                                  {
                                      &mut [][..]
                                  }
                                  else
                                  {
                                      if call.pixel_length > crate::display::PIXELS
                           || call.pixels.is_null()
                           || !(call.pixels as usize).is_multiple_of(core::mem::align_of::<u32>())
                        {
                            return Err(BAD_ARGUMENT);
                        }
                                      unsafe {
                                          core::slice::from_raw_parts_mut(call.pixels,
                                                                          call.pixel_length)
                                      }
                                  };
                                  session.worlds.request(&ledger, call, pixels)?;
                              }
                              return Ok(Some(()));
                          }
                          let DrawingTarget::World(id) = target
                          else
                          {
                              return Err(BAD_ARGUMENT);
                          };
                          let world = session.worlds.get_mut(id.0, owner)?;
                          match number
                          {
                              QUICKDRAW =>
                              {
                                  let call =
                                      unsafe { super::request::<GraphicsCall>(pointer, size) }?;
                                  if !call.surface.is_null()
                                  {
                                      return Err(BAD_ARGUMENT);
                                  }
                                  let points =
                                      unsafe { input(call.points, call.point_count, 4096) }?;
                                  let region =
                                      unsafe { input(call.region, call.region_length, 65535) }?;
                                  world.graphics(call, points, region)?;
                              }
                              FONT =>
                              {
                                  let call = unsafe { super::request::<FontCall>(pointer, size) }?;
                                  if !call.surface.is_null()
                                  {
                                      return Err(BAD_ARGUMENT);
                                  }
                                  let bytes = unsafe { input(call.text, call.text_length, 65535) }?;
                                  let text = core::str::from_utf8(bytes).map_err(|_| BAD_ARGUMENT)?;
                                  if call.operation == FONT_DRAW
                                  {
                                      world.text(call, text, &session.fonts)?;
                                  }
                                  else
                                  {
                                      let status =
                            crate::toolbox::quickdraw::fonts_service::query(&session.fonts, call,
                                                                            text);
                                      if status != OK
                                      {
                                          return Err(status);
                                      }
                                  }
                              }
                              COPY_BITS =>
                              {
                                  let call =
                                      unsafe { super::request::<BitmapCall>(pointer, size) }?;
                                  if !call.surface.is_null()
                                  {
                                      return Err(BAD_ARGUMENT);
                                  }
                                  world.bitmap(call,
                                               unsafe {
                                                   input(call.data,
                                                         call.data_length,
                                                         2 * 1024 * 1024)
                                               }?,
                                               unsafe {
                                                   input(call.palette, call.palette_length, 256)
                                               }?,
                                               unsafe {
                                                   input(call.mask, call.mask_length, 65535)
                                               }?)?;
                              }
                              _ => return Err(NOT_SUPPORTED)
                          }
                          Ok(Some(()))
                      })
    })();
    match result
    {
        Ok(None) => None,
        Ok(Some(())) => Some(OK as i64),
        Err(status) => Some(status as i64)
    }
}
unsafe fn input<'a, T>(pointer: *const T, length: usize, maximum: usize)
                       -> Result<&'a [T], Status>
{
    if length == 0
    {
        return Ok(&[]);
    }
    if length > maximum
       || pointer.is_null()
       || !(pointer as usize).is_multiple_of(core::mem::align_of::<T>())
    {
        return Err(BAD_ARGUMENT);
    }
    Ok(unsafe { core::slice::from_raw_parts(pointer, length) })
}
