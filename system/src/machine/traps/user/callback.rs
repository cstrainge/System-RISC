//! The current canonical callback THRD alone receives and returns its work.
//! Delivery commits only when the enclosing checked request finishes copyout.
use mac_abi::{definitions::{self as abi, CallbackCall},
              *};

use super::{Call, CallbackInput};
use crate::{machine::scheduler::{callbacks,
                                 service::{RequestAction, RequestResult}},
            toolbox::resources::definitions::callbacks as broker};

impl Call
{
    pub(crate) fn definition_panic(&mut self, frame: &crate::machine::traps::TrapFrame)
                                   -> Result<(), Status>
    {
        if self.number != mac_abi::traps::DEFINITION_PANIC
           || self.size != core::mem::size_of::<abi::DefinitionPanic>()
           || crate::context::current_thread_context().identity().thread != self.thread
        {
            return Err(ACCESS_DENIED);
        }
        let execution = callbacks::authenticate_current()?;
        let identity = broker::panic_identity(execution.queue(), &execution)?;
        // This pointer is the original field-validated owned request snapshot,
        // not the user address or a copied invocation selector.
        let panic = unsafe { &*self.owned_mut(0)?.pointer().cast::<abi::DefinitionPanic>() };
        crate::machine::scheduler::capture_definition_panic(frame, &execution, &identity, panic)
    }

    pub(crate) fn dispatch_callback(&mut self, suspension: Result<(), Status>) -> RequestResult
    {
        match self.callback_operation(suspension)
        {
            Ok(action) =>
            {
                RequestResult { status: OK,
                                action }
            }
            Err(status) => RequestResult::done(status)
        }
    }

    fn callback_operation(&mut self, suspension: Result<(), Status>)
                          -> Result<RequestAction, Status>
    {
        let (operation, result) = {
            // prepare_or_resume validated this exact layout; callback_buffer
            // below additionally checks the original captured dynamic span.
            let call = unsafe { &*self.owned_mut(0)?.pointer().cast::<CallbackCall>() };
            (call.operation, call.result)
        };
        let (_, capacity) = self.callback_buffer(operation)?;
        let execution = callbacks::authenticate_current()?;
        let queue = execution.queue();
        match operation
        {
            abi::CALLBACK_NEXT =>
            {
                suspension?;
                match broker::next(queue, &execution, capacity)?
                {
                    broker::Next::Wait =>
                    {
                        // Readiness is an atomic read under the actual scheduler
                        // park turn. A producer publishes it before its wake.
                        callbacks::park_current(&execution, || queue.ready())?;
                        Ok(RequestAction::Replay)
                    }
                    broker::Next::Required(bytes) =>
                    {
                        let call =
                            unsafe { &mut *self.owned_mut(0)?.pointer().cast::<CallbackCall>() };
                        call.required = bytes;
                        call.present = 0;
                        Ok(RequestAction::Return)
                    }
                    broker::Next::Work(delivery) =>
                    {
                        self.attach_delivery(delivery).map_err(|(status, delivery)| {
                                                           // Outside every scheduler/broker gate.
                                                           // Drop rolls
                                                           // back this same not-yet-delivered
                                                           // original job.
                                                           drop(delivery);
                                                           status
                                                       })?;
                        Ok(RequestAction::Return)
                    }
                }
            }
            abi::CALLBACK_RETURN =>
            {
                let bytes = self.take_callback_input()?;
                match bytes
                {
                    CallbackInput::Ordinary(bytes) =>
                    {
                        broker::returned(queue, &execution, bytes, result)
                            .map_err(|(status, bytes)| { drop(bytes); status })?;
                    }
                    CallbackInput::Prepaid(loan) =>
                    {
                        broker::returned_prepaid(queue, &execution, loan, result)
                            .map_err(|(status, loan)| { drop(loan); status })?;
                    }
                }
                Ok(RequestAction::Return)
            }
            _ => Err(BAD_ARGUMENT)
        }
    }
}
