//! Slot backing is acquired before THRD publication. GET/SET/NEXT/ACK use no
//! allocator; the in-flight receipt retains its original key and code lifetime.
use super::*;
#[derive(Clone, Copy, Default)]
struct Slot
{
    key:      u64,
    value:    usize,
    eligible: u32
}
#[derive(Clone, Copy)]
enum Phase
{
    Active,
    Draining
    {
        round:  u32,
        cursor: usize,
        done:   bool
    },
    Finished
}
struct Drain
{
    token: u64,
    key:   KeyRead
}
pub(crate) struct ThreadSlots
{
    slots:     Vec<Slot>,
    drain:     Option<Drain>,
    phase:     Phase,
    root:      Arc<Root>,
    // Follows every actual array/key/root pin, including forced-stop teardown.
    _progress: Progress
}
impl Drop for ThreadSlots
{
    fn drop(&mut self)
    {
        // Forced owner retirement never pretends an outstanding user callback
        // completed. The THRD/claim barrier proves it cannot execute again.
        if let Some(drain) = self.drain.take()
        {
            drop(drain);
        }
        // Final slot disposal also makes previously detached keys eligible.
        // Context release may still hold scheduler/IRQ scopes: schedule only.
        request_reap();
    }
}
pub(crate) fn prepare_slots(owner: &OwnerRef, binding: &RootBinding) -> Result<ThreadSlots, Error>
{
    if !owner.accepting()
    {
        return Err(Error::Finished);
    }
    let root = root(binding, owner)?;
    root.kernel(|| {
            let mut slots = Vec::new();
            let capacity = crate::machine::memory::TLS_KEY_CAPACITY as usize;
            slots.try_reserve_exact(capacity)
                 .map_err(|_| Error::Metadata("actual thread slots"))?;
            slots.resize(capacity, Slot::default());
            Ok(ThreadSlots { slots,
                             drain: None,
                             phase: Phase::Active,
                             root: root.clone(),
                             _progress: Progress { owner: owner.clone() } })
        })
}
impl ThreadSlots
{
    pub(crate) fn owner(&self) -> DomainId
    {
        self.root.owner.owner()
    }

    pub(crate) fn finished(&self) -> bool
    {
        matches!(self.phase, Phase::Finished)
    }

    pub(super) fn root(&self) -> &Arc<Root>
    {
        &self.root
    }

    fn active(&self) -> Result<(), Error>
    {
        if matches!(self.phase, Phase::Finished) { Err(Error::Finished) } else { Ok(()) }
    }

    pub(super) fn get(&mut self, key: &KeyRead, id: u64) -> Result<usize, Error>
    {
        self.active()?;
        if !Arc::ptr_eq(&self.root, &key.position.root)
        {
            return Err(Error::Authority);
        }
        let slot = self.slots.get(key.position.at).ok_or(Error::Invalid)?;
        Ok(if slot.key == id { slot.value } else { 0 })
    }

    pub(super) fn set(&mut self, key: &KeyRead, id: u64, value: usize) -> Result<(), Error>
    {
        self.active()?;
        if !Arc::ptr_eq(&self.root, &key.position.root)
        {
            return Err(Error::Authority);
        }
        let eligible = match self.phase
        {
            Phase::Draining { round,
                              .. } => round + 1,
            _ => 0
        };
        let slot = self.slots.get_mut(key.position.at).ok_or(Error::Invalid)?;
        *slot = Slot { key: id,
                       value,
                       eligible };
        if let Phase::Draining { done,
                                 .. } = &mut self.phase
        {
            if value != 0
            {
                *done = false;
            }
        }
        Ok(())
    }

    pub(super) fn begin(&mut self) -> Result<(), Error>
    {
        if !matches!(self.phase, Phase::Active) || self.drain.is_some()
        {
            return Err(Error::Busy);
        }
        self.phase = Phase::Draining { round:  0,
                                       cursor: 0,
                                       done:   false };
        Ok(())
    }

    /// Clears before returning the callback; no user callback is invoked here.
    pub(super) fn next(&mut self, call: &mut mac_abi::tls::Call) -> Result<(), Error>
    {
        if self.drain.is_some()
        {
            return Err(Error::Busy);
        }
        let Phase::Draining { mut round,
                              mut cursor,
                              done } = self.phase
        else
        {
            return Err(Error::Invalid);
        };
        if done
        {
            return Ok(());
        }
        while round < mac_abi::tls::DESTRUCTOR_ROUNDS
        {
            while cursor < self.slots.len()
            {
                let at = cursor;
                cursor += 1;
                self.phase = Phase::Draining { round,
                                               cursor,
                                               done: false };
                let slot = self.slots[at];
                if slot.value == 0 || slot.eligible > round
                {
                    continue;
                }
                let actual = match key(&self.root.owner, slot.key)
                {
                    Ok(key) => key,
                    Err(Error::Missing) =>
                    {
                        self.slots[at].value = 0;
                        continue;
                    }
                    Err(cause) =>
                    {
                        self.phase = Phase::Draining { round,
                                                       cursor: at,
                                                       done: false };
                        return Err(cause);
                    }
                };
                if actual.position.at != at || !Arc::ptr_eq(&self.root, &actual.position.root)
                {
                    return Err(Error::Authority);
                }
                if actual.destructor == 0
                {
                    self.slots[at].value = 0;
                    continue;
                }
                let token = match mac_object_table::domain::issue_correlation_identity()
                {
                    Ok(token) => token.get(),
                    Err(_) =>
                    {
                        self.phase = Phase::Draining { round,
                                                       cursor: at,
                                                       done: false };
                        return Err(Error::Identity);
                    }
                };
                // Single outstanding THRD-owned protocol correlation. The token
                // alone grants no key/code authority and ACK cannot recreate it.
                self.slots[at].value = 0;
                call.present = 1;
                call.key = slot.key;
                call.value = slot.value;
                call.destructor = actual.destructor;
                call.ticket = token;
                call.round = round + 1;
                self.drain = Some(Drain { token,
                                          key: actual });
                return Ok(());
            }
            round += 1;
            cursor = 0;
            self.phase = Phase::Draining { round,
                                           cursor,
                                           done: false };
            if !self.remaining()?
            {
                self.phase = Phase::Draining { round,
                                               cursor,
                                               done: true };
                return Ok(());
            }
        }
        let remaining = self.slots.iter().filter(|slot| slot.value != 0).count();
        Err(Error::Rounds { remaining })
    }

    fn remaining(&mut self) -> Result<bool, Error>
    {
        let mut remaining = false;
        for slot in self.slots.iter_mut()
        {
            if slot.value == 0
            {
                continue;
            }
            match key(&self.root.owner, slot.key)
            {
                Ok(key) if key.destructor != 0 => remaining = true,
                Ok(_) | Err(Error::Missing) => slot.value = 0,
                Err(cause) => return Err(cause)
            }
        }
        Ok(remaining)
    }

    /// Return the original pin to be destroyed after the context guard ends.
    pub(super) fn ack(&mut self, token: u64) -> Result<KeyRead, Error>
    {
        if self.drain.as_ref().is_none_or(|drain| token == 0 || drain.token != token)
        {
            return Err(Error::Ticket);
        }
        let Some(drain) = self.drain.take()
        else
        {
            return Err(Error::Ticket);
        };
        Ok(drain.key)
    }

    pub(super) fn finish(&mut self) -> Result<(), Error>
    {
        if self.drain.is_some()
        {
            return Err(Error::Busy);
        }
        if !matches!(self.phase,
                     Phase::Draining { done: true,
                                       .. })
        {
            return Err(Error::Invalid);
        }
        self.phase = Phase::Finished;
        Ok(())
    }
}
