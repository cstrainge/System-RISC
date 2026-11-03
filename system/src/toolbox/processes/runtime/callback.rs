//! Admit a callback thread from a retained source and its actual Process entry.
#![forbid(unsafe_code)]
use mac_abi::{ACCESS_DENIED, Status};

impl crate::toolbox::runtime::Toolbox
{
    pub(crate) fn callback_thread_preflight(
        &self, source: &crate::images::Read)
        -> Result<(crate::toolbox::handles::OwnerRef,
                   crate::machine::scheduler::accounting::ThreadCharges,
                   crate::toolbox::application_heap::RootBinding,
                   crate::toolbox::native_threads::RootBinding),
                  Status>
    {
        let owner = source.owner();
        let binding = self.processes.resource_binding(owner.owner().incarnation())?;
        if binding.ledger().owner() != owner.owner()
           || binding.image() != Some(source.binding())
           || !owner.accepting()
        {
            return Err(ACCESS_DENIED);
        }
        let tls = self.processes.native_exit_preflight(owner, source.binding())?;
        Ok((owner.clone(),
            self.processes.thread_charges(binding.owner())?,
            self.processes.heap(binding.owner())?,
            tls))
    }
}
