//! Real, guard-free canonical CTRL disposal maintenance. Only the admitted
//! active list is visited after a real callback/source/manager return edge.
#![forbid(unsafe_code)]
use crate::toolbox::controls::objects::ControlHost;
fn desktop<R>(run: impl FnOnce(&mut crate::toolbox::runtime::Toolbox) -> R)
              -> Result<R, crate::state::AccessError>
{
    super::with_window_toolbox_owned((), |(), toolbox| run(toolbox)).map_err(|(cause, ())| cause)
}
pub(super) fn pump()
{
    if !super::TOOLBOX.control_work_ready()
    {
        return;
    }
    let work = desktop(|toolbox| {
        if super::desktop::take_control_source_returned()
        {
            toolbox.managed_controls().disposal_sources_returned();
        }
        if super::desktop::take_control_acknowledged()
        {
            toolbox.managed_controls().disposal_acknowledgements();
        }
        if !toolbox.managed_controls().take_disposal_notice()
        {
            return None;
        }
        toolbox.managed_controls().disposal_work()
    });
    let Ok(mut current) = work
    else
    {
        return;
    };
    while let Some(slot) = current
    {
        current = slot.next();
        if let Err(status) = slot.advance()
        {
            serial!("CONTROL_DISPOSAL_RETAINED status={status}");
        }
        slot.deliver_wake();
    }
    if !matches!(desktop(|toolbox| toolbox.retire_user_controls()), Ok(Ok(())))
    {
        return;
    }
    let Ok(mut current) = desktop(|toolbox| toolbox.managed_controls().disposal_work())
    else
    {
        return;
    };
    while let Some(slot) = current
    {
        current = slot.next();
        slot.drain();
        slot.deliver_wake();
    }
    loop
    {
        let Ok(retired) = desktop(|toolbox| toolbox.managed_controls().take_finished_disposal())
        else
        {
            return;
        };
        let Some(retired) = retired
        else
        {
            return;
        };
        // next was detached. Metadata/source destruction never recursively
        // drops the work list or runs under the Desktop manager exclusion.
        drop(retired);
    }
}
