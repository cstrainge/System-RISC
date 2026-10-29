use mac_abi::controls::reflow as wire;

use super::*;

impl State
{
    pub(super) fn reflow(&mut self, payload: &mut wire::Payload, entry: DefinitionEntry,
                         gate: TrapGate)
                         -> Result<bool, Status>
    {
        let request = layout::decode_leaf(payload).map_err(|e| e.status())?;
        let layout = self.layout.as_mut().ok_or(NOT_SUPPORTED)?;
        payload.creation = layout.sizing.creation().into();
        let request = match layout.sizing.prepare_message(request)
        {
            layout::Outcome::Handled { reply,
                                       effects } =>
            {
                // Scalar effects need no queue allocation: this leaf has no parent.
                // The root/container scheduler will consume pending arrangement.
                reply.commit();
                layout.arrangement_pending |= effects.arrangement;
                return Ok(false);
            }
            layout::Outcome::Failed { cause,
                                      .. } => return Err(cause.status()),
            layout::Outcome::Unhandled(request) => request
        };
        if let layout::Message::AskSize(constraints) = request
        {
            let measured = if let Some((key, measured)) = layout.measured
                              && key == constraints
            {
                measured
            }
            else
            {
                let mut state = self.spec.draw();
                super::super::reflow::invoke(&mut state, payload, entry, gate)?;
                let measured =
                    layout::Measurement::try_from(payload.measurement).map_err(|_| BAD_ABI)?;
                let change = layout.sizing.prepare_calculated(layout::SizePreferences {
                    minimum: Some(measured.intrinsic.minimum()), preferred: Some(measured.intrinsic.preferred())
                }).commit();
                layout.arrangement_pending |= change.effects.arrangement;
                layout.measured = Some((constraints, measured));
                measured
            };
            payload.measurement = layout::Measurement {
                intrinsic: layout.sizing.resolve(measured.intrinsic),
                constrained: layout.sizing.resolve(measured.constrained)
            }.into();
            return Ok(false);
        }
        if let layout::Message::AssignRect(rect) = request
           && rect.rect() == self.spec.bounds()
        {
            layout.arrangement_pending = false;
            return Ok(false);
        }
        if let layout::Message::SetVisibleRect(rect) = request
           && layout.visible == Some(rect.rect())
        {
            return Ok(false);
        }
        let mut state = self.spec.draw();
        super::super::reflow::invoke(&mut state, payload, entry, gate)?;
        match request
        {
            layout::Message::AssignRect(rect) =>
            {
                self.spec.set_bounds(rect.rect());
                self.tracking = None;
                self.spec.set_highlight(0);
                layout.arrangement_pending = false;
                Ok(false)
            }
            layout::Message::SetVisibleRect(rect) =>
            {
                layout.visible = Some(rect.rect());
                self.tracking = None;
                self.spec.set_highlight(0);
                Ok(true)
            }
            layout::Message::InvalidateLayout(invalidation) =>
            {
                let bits = u32::from(invalidation);
                if bits & wire::INVALIDATE_MEASUREMENT != 0
                {
                    layout.measured = None;
                }
                layout.arrangement_pending |= bits & wire::INVALIDATE_ARRANGEMENT != 0;
                Ok(false)
            }
            _ => Err(NOT_SUPPORTED)
        }
    }
}
