
use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap, HashSet},
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

static NEXT_FAILURE: AtomicU64 = AtomicU64::new(1);
static NEXT_REGION: AtomicU64 = AtomicU64::new(1);

use crate::language::{
    bytecode::CompiledTry,
    data::{
        failure::{
            ControlFailure, FailureOccurrence, FailureStatus, RelatedFailure, marked, occurrence,
            pending,
        },
        methods::{conversion_key, method_key},
        scoped_variables::{ScopedValue, ValueVisibility},
        types::{StructValue, TypeId, TypeKind},
        value::Value,
    },
    interpreter::{CallFrame, ErrorWhat, Interpreter, InterpreterError, InterpreterResult},
    text::location::Location,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Transfer
{
    Return,
    Break,
    Continue,
}

#[derive(Clone)]
pub(super) struct CleanupGuard
{
    pub hook: bool,
    pub deadline: Instant,
    pub instructions: usize,
    pub function_depth: usize,
    pub interrupted: bool,
}

#[derive(Default)]
pub(super) struct ExceptionState
{
    pub regions: Vec<u64>,
    pub readers: Vec<(u64, Rc<super::streams::Reader>)>,
    pub resources: Vec<(u64, crate::language::data::native_value::NativeValue)>,
    pub next_region: u64,
    pub next_failure: u64,
    pub pending: BTreeMap<u64, Value>,
    pub bindings: HashMap<usize, u64>,
    pub captures: HashSet<usize>,
    pub frames: Vec<CallFrame>,
    pub returns: Vec<Option<TypeId>>,
    pub callers: Vec<String>,
    pub json_calls: usize,
    pub json_providers: HashSet<TypeId>,
    pub transfer: Option<Transfer>,
    pub cleanup: Option<CleanupGuard>,
    pub transfer_cleanup: Option<(usize, CleanupGuard)>,
    pub operation: Option<(Vec<std::sync::Arc<std::sync::atomic::AtomicBool>>, Instant)>,
    pub protected: Vec<usize>,
    pub last_location: Location,
    pub reporting: bool,
    pub readline_callback: bool,
    pub report_floor: usize,
    pub report_prefix: Option<String>,
}

impl Interpreter
{
    pub(super) fn enter_failure_region(&mut self) -> u64
    {
        self.exceptions.next_region = NEXT_REGION.fetch_add(1, Ordering::Relaxed);
        let region = self.exceptions.next_region;
        self.exceptions.regions.push(region);
        region
    }

    pub(super) fn activate_failure(
        &mut self,
        location: &Location,
        value: &mut Value,
        reactivate: bool,
    ) -> InterpreterResult<()>
    {
        if let Value::Named(item) = value
        {
            return self.activate_failure(location, &mut Rc::make_mut(item).value, reactivate);
        }
        let Value::Struct(item) = value
        else
        {
            return Ok(());
        };
        if !item.definition.error
        {
            return Ok(());
        }
        if item
            .failure
            .as_ref()
            .is_some_and(|failure| failure.borrow().status == FailureStatus::Pending)
        {
            return Ok(());
        }
        if item
            .failure
            .as_ref()
            .is_some_and(|failure| failure.borrow().id != 0)
            && !reactivate
        {
            return Ok(());
        }
        let previous = item.failure.as_ref().map(|failure| {
            let failure = failure.borrow();
            (
                failure.origin.clone(),
                failure.frames.clone(),
                failure.formatter.clone(),
                failure.related.clone(),
                failure.propagation.clone(),
                failure.native_origin.clone(),
                failure.control,
            )
        });
        let rethrow_source = if reactivate
        {
            item.failure
                .as_ref()
                .map(|failure| failure.borrow().id)
                .filter(|id| *id != 0)
        }
        else
        {
            None
        };
        let constructing = item.constructing;
        let id = if constructing
        {
            0
        }
        else
        {
            self.exceptions.next_failure = NEXT_FAILURE.fetch_add(1, Ordering::Relaxed);
            self.exceptions.next_failure
        };
        let owner = *self.exceptions.regions.last().unwrap_or(&0);
        let string = self.scope().types.builtin_id("String").unwrap();
        let name = self
            .scope()
            .types
            .conversion_method_name(item.definition.id);
        let typed = conversion_key(string, &name, item.definition.id);
        let formatter = self
            .scope()
            .lexical_function(&typed)
            .or_else(|| self.scope().imported_methods.get(&typed).cloned())
            .or_else(|| self.scope().lexical_function(&method_key(string, &name)))
            .or_else(|| {
                self.scope()
                    .imported_methods
                    .get(&method_key(string, &name))
                    .cloned()
            });
        let (origin, frames, formatter, related, mut propagation, native_origin, control) =
            previous.unwrap_or_else(|| {
                (
                    location.clone(),
                    self.exceptions
                        .frames
                        .iter()
                        .rev()
                        .take(128)
                        .cloned()
                        .collect(),
                    formatter,
                    Vec::new(),
                    Vec::new(),
                    None,
                    None,
                )
            });
        if reactivate && id != 0 && origin != *location
        {
            propagation.push(location.clone());
        }
        Rc::make_mut(item).failure = Some(Rc::new(RefCell::new(FailureOccurrence {
            id,
            owner,
            control,
            rethrow_source,
            status: if constructing
            {
                FailureStatus::Acknowledged
            }
            else
            {
                FailureStatus::Pending
            },
            origin,
            frames,
            formatter,
            related,
            propagation,
            native_origin,
            slots: HashSet::new(),
        })));
        if id != 0
        {
            self.exceptions.pending.insert(id, value.clone());
        }
        Ok(())
    }

    pub(super) fn consume_success(
        &mut self,
        location: &Location,
        value: &Value,
    ) -> InterpreterResult<()>
    {
        if pending(value).is_some()
        {
            return Err(self.raise_failure(location, value.clone())?);
        }
        Ok(())
    }

    pub(super) fn prepare_failure_value(
        &mut self,
        location: &Location,
        value: &mut Value,
        expected: Option<TypeId>,
    ) -> InterpreterResult<()>
    {
        let capture = expected.is_some_and(|id| self.scope().types.has_error_type(id));
        if pending(value).is_some()
        {
            let compatible = expected.is_some_and(|id| {
                self.scope()
                    .types
                    .catch_matches(id, self.scope().types.value_type(value.underlying()))
            });
            if !compatible
            {
                return Err(self.raise_failure(location, value.clone())?);
            }
        }
        if !capture
            && let Value::Named(item) = value
            && self.scope().types.has_error_type(item.definition.id)
        {
            *value = item.value.clone();
        }
        Ok(())
    }

    pub(super) fn register_failure_binding(&mut self, name: &str)
    {
        if let Some(binding) = self.variable_binding(name)
        {
            let key = Rc::as_ptr(&binding) as usize;
            let owner = *self.exceptions.regions.last().unwrap_or(&0);
            self.exceptions.bindings.insert(key, owner);
            self.exceptions.captures.remove(&key);
            if let Some(item) = pending(&binding.borrow().value)
            {
                let mut item = item.borrow_mut();
                if item.owner == owner
                {
                    item.slots.insert(key);
                }
            }
        }
    }

    pub(super) fn check_failure_write(
        &mut self,
        location: &Location,
        binding: &Rc<RefCell<ScopedValue>>,
        value: &Value,
    ) -> InterpreterResult<()>
    {
        let key = Rc::as_ptr(binding) as usize;
        crate::runtime::tasks::check_write(key).map_err(|error| {
            InterpreterError::new(location.clone(), ErrorWhat::ReceiverBusy(error.message))
        })?;
        let old = binding.borrow().value.clone();
        if let Some(item) = pending(value)
        {
            let owner = item.borrow().owner;
            let target = self.exceptions.bindings.get(&key).copied();
            let position = self
                .exceptions
                .regions
                .iter()
                .position(|region| *region == owner);
            let target = target.and_then(|target| {
                self.exceptions
                    .regions
                    .iter()
                    .position(|region| *region == target)
            });
            if self.exceptions.captures.contains(&key)
                || target
                    .zip(position)
                    .is_none_or(|(target, owner)| target < owner)
            {
                return Err(self.raise_failure(location, value.clone())?);
            }
        }
        if let Some(item) = pending(&old)
        {
            let replacing = pending(value).is_some_and(|new| Rc::ptr_eq(&new, &item));
            if !replacing && item.borrow().slots.contains(&key) && item.borrow().slots.len() == 1
            {
                return Err(self.raise_failure(location, old)?);
            }
        }
        Ok(())
    }

    pub(super) fn commit_failure_write(&mut self, binding: &Rc<RefCell<ScopedValue>>, old: &Value)
    {
        let key = Rc::as_ptr(binding) as usize;
        if let Some(item) = occurrence(old)
        {
            item.borrow_mut().slots.remove(&key);
        }
        if let Some(item) = pending(&binding.borrow().value)
        {
            let mut item = item.borrow_mut();
            if self.exceptions.bindings.get(&key) == Some(&item.owner)
            {
                item.slots.insert(key);
            }
        }
    }

    pub(super) fn raise_failure(
        &mut self,
        location: &Location,
        value: Value,
    ) -> InterpreterResult<InterpreterError>
    {
        if !marked(&value)
        {
            return Err(InterpreterError::new(
                location.clone(),
                ErrorWhat::InvalidOperand("throw requires a marked error value".into()),
            ));
        }
        let value = value.underlying().clone();
        let failure = occurrence(&value).ok_or_else(|| {
            InterpreterError::new(
                location.clone(),
                ErrorWhat::InvalidOperand("An incomplete error cannot be thrown".into()),
            )
        })?;
        let mut failure = failure.borrow_mut();
        failure.status = FailureStatus::Propagating;
        self.exceptions.pending.remove(&failure.id);
        let what = match failure.control
        {
            Some(ControlFailure::Interrupted) => ErrorWhat::Interrupted,
            Some(ControlFailure::CleanupAbort) => ErrorWhat::CleanupAbort,
            Some(ControlFailure::CleanupTimeout) => ErrorWhat::CleanupTimeout,
            None => ErrorWhat::Raised(public_summary(&value)),
        };
        let mut error = InterpreterError::new(failure.origin.clone(), what);
        error.trace.native_location = failure.native_origin.clone();
        error.trace.call_stack = failure.frames.clone();
        error.trace.related = failure.related.clone();
        error.trace.raised = Some(value.clone());
        error.trace.rethrow_source = failure.rethrow_source;
        drop(failure);
        error.trace.resolved.push(occurrence(&value).unwrap());
        Ok(error)
    }

    pub(super) fn leave_failure_region(
        &mut self,
        region: u64,
        mut result: InterpreterResult<()>,
        function: bool,
    ) -> InterpreterResult<()>
    {
        while self
            .exceptions
            .regions
            .last()
            .is_some_and(|id| *id != region)
        {
            let child = *self.exceptions.regions.last().unwrap();
            result = self.leave_scope_region(child, result, false, false);
        }
        self.leave_scope_region(region, result, function, false)
    }

    pub(super) fn leave_scope_region(
        &mut self,
        region: u64,
        mut result: InterpreterResult<()>,
        function: bool,
        expression: bool,
    ) -> InterpreterResult<()>
    {
        // Freeze runtime occurrences before any release callback can redefine methods or
        // mutate diagnostic context. Later unwinding uses this packet rather than repinning.
        if let Err(error) = &mut result
            && error.trace.raised.is_none()
        {
            let value = self.caught_value(error);
            if let Some(failure) = occurrence(&value)
            {
                error.trace.call_stack = failure.borrow().frames.clone();
                failure.borrow_mut().status = FailureStatus::Propagating;
                error.trace.resolved.push(failure);
                error.trace.raised = Some(value);
            }
        }
        let returning =
            function || expression || self.exceptions.transfer == Some(Transfer::Return);
        let mut escrow = None;
        if result.is_ok() && returning
        {
            let mut value = self.last_result.take().unwrap_or(Value::None);
            if marked(&value) && (!expression || pending(&value).is_some())
            {
                if let Value::Struct(item) = &value
                {
                    let definition = item.definition.clone();
                    let mut registry = self.scope().types.clone();
                    registry.allow_candidates = false;
                    result = registry.validate(definition.id, &value).map_err(|message| {
                        InterpreterError::new(
                            definition.location.clone().unwrap_or_default(),
                            ErrorWhat::ArgumentMismatch(message),
                        )
                    });
                    if let Value::Struct(item) = &mut value
                    {
                        Rc::make_mut(item).constructing = false;
                    }
                }
                if result.is_ok()
                {
                    self.activate_failure(
                        &self.exceptions.last_location.clone(),
                        &mut value,
                        !expression,
                    )?;
                    let allowed = expression
                        || self
                            .exceptions
                            .returns
                            .last()
                            .copied()
                            .flatten()
                            .is_some_and(|id| {
                                self.scope().types.catch_matches(
                                    id,
                                    self.scope().types.value_type(value.underlying()),
                                )
                            });
                    if allowed
                    {
                        escrow = Some(value.clone());
                    }
                    else
                    {
                        result = Err(self.raise_failure(&Location::default(), value.clone())?);
                    }
                }
            }
            self.last_result = Some(value);
        }
        let escrow_id = escrow
            .as_ref()
            .and_then(occurrence)
            .map(|item| item.borrow().id);
        let outstanding: Vec<_> = self
            .exceptions
            .pending
            .values()
            .filter(|value| {
                pending(value).is_some_and(|item| {
                    item.borrow().owner == region && Some(item.borrow().id) != escrow_id
                })
            })
            .cloned()
            .collect();
        if !outstanding.is_empty()
            && result.is_ok()
            && let Some(value) = escrow.take()
        {
            result = Err(self.raise_failure(&Location::default(), value)?);
        }
        for value in outstanding
        {
            let raised = self.raise_failure(&Location::default(), value)?;
            result = self.combine_failure(result, Err(raised), "Unhandled");
        }
        let abort = result
            .as_ref()
            .err()
            .is_some_and(|error| error.trace.abort_cleanup);
        if abort && let Some(reactor) = crate::runtime::tasks::current_reactor()
        {
            reactor.abort_tasks();
        }
        let inherited = self.exceptions.cleanup.is_some();
        let releasing = result.is_err() || self.exceptions.regions.len() == 1;
        if !inherited && releasing
        {
            let previous = result
                .as_mut()
                .err()
                .and_then(|error| error.trace.cleanup.take());
            self.exceptions.cleanup = Some(previous.unwrap_or_else(|| {
                CleanupGuard {
                    hook: false,
                    deadline: Instant::now() + Duration::from_secs(5),
                    instructions: 1_000_000,
                    function_depth: self.exceptions.frames.len(),
                    interrupted: result
                        .as_ref()
                        .err()
                        .is_some_and(|error| matches!(error.what, ErrorWhat::Interrupted)),
                }
            }));
        }
        let parent = self.exceptions.regions.iter().rev().nth(1).copied();
        if result.is_ok()
            && let Some(parent) = parent
        {
            for (owner, _) in &mut self.exceptions.resources
            {
                if *owner == region
                {
                    *owner = parent;
                }
            }
        }
        else
        {
            let mut release = Vec::new();
            self.exceptions.resources.retain(|(owner, value)| {
                if *owner == region
                {
                    release.push(value.clone());
                    false
                }
                else
                {
                    true
                }
            });
            for value in release.into_iter().rev()
            {
                value.0.release();
            }
        }
        let cleanup = self.release_stream_region(region, result.is_err(), abort);
        result = self.combine_failure(result, cleanup, "Cleanup");
        if crate::runtime::tasks::active()
            && let Some(reactor) = crate::runtime::tasks::current_reactor()
        {
            let deadline = Instant::now() + Duration::from_secs(5);
            while reactor.cancelled_tasks_pending() && Instant::now() < deadline
            {
                if crate::runtime::tasks::cooperate().is_err()
                {
                    break;
                }
            }
        }
        if !inherited && releasing
        {
            let budget = self.exceptions.cleanup.take();
            if let Err(error) = &mut result
            {
                error.trace.cleanup = budget;
            }
        }
        self.exceptions.regions.pop();
        if result.is_ok()
            && let Some(value) = escrow
            && let Some(item) = occurrence(&value)
        {
            let mut item = item.borrow_mut();
            if item.owner == region
            {
                item.owner = *self.exceptions.regions.last().unwrap_or(&0);
                item.slots.clear();
            }
        }
        self.exceptions.bindings.retain(|_, owner| *owner != region);
        if result.is_err()
        {
            self.exceptions.transfer = None;
        }
        result
    }

    pub(super) fn leave_control_scopes(&mut self, depth: usize) -> InterpreterResult<()>
    {
        let mut result = Ok(());
        while self.exceptions.regions.len() > depth
        {
            let region = *self.exceptions.regions.last().unwrap();
            result = self.leave_scope_region(region, result, false, false);
        }
        result
    }

    pub(super) fn combine_failure(
        &mut self,
        primary: InterpreterResult<()>,
        secondary: InterpreterResult<()>,
        relation: &str,
    ) -> InterpreterResult<()>
    {
        match (primary, secondary)
        {
            (Err(mut primary), Err(secondary)) =>
            {
                if primary.trace.related.len() < 16
                {
                    primary
                        .trace
                        .related
                        .push(self.related_failure(&secondary, relation));
                }
                primary.trace.abort_cleanup |= secondary.trace.abort_cleanup;
                primary.trace.resolved.extend(secondary.trace.resolved);
                Err(primary)
            }
            (Err(primary), _) => Err(primary),
            (_, result) => result,
        }
    }

    pub(super) fn conversion_causes(
        &mut self,
        location: &Location,
        sources: &[Value],
        result: &mut InterpreterResult<()>,
    )
    {
        let mut seen = HashSet::new();
        for source in sources.iter().filter(|source| marked(source))
        {
            let Some(item) = occurrence(source)
            else
            {
                continue;
            };
            let id = item.borrow().id;
            if !seen.insert(id)
            {
                continue;
            }
            let source_error = self.raise_failure(location, source.clone());
            item.borrow_mut().status = FailureStatus::Acknowledged;
            let Ok(source_error) = source_error
            else
            {
                continue;
            };
            let related = self.related_failure(&source_error, "Cause");
            match result
            {
                Err(error) =>
                {
                    if error.trace.related.len() < 16
                    {
                        error.trace.related.push(related);
                    }
                }
                Ok(()) =>
                {
                    if let Some(mut value) = self.last_result.take()
                    {
                        if occurrence(&value).is_some_and(|item| item.borrow().id == id)
                        {
                            let _ = self.activate_failure(location, &mut value, true);
                        }
                        if let Some(item) = occurrence(&value)
                        {
                            let mut item = item.borrow_mut();
                            if item.related.len() < 16
                            {
                                item.related.push(related);
                            }
                        }
                        self.last_result = Some(value);
                    }
                }
            }
        }
    }

    pub(super) fn check_cleanup_budget(&mut self, location: &Location) -> InterpreterResult<()>
    {
        if let Some(guard) = &mut self.exceptions.cleanup
        {
            if guard.instructions == 0 || Instant::now() >= guard.deadline
            {
                return Err(InterpreterError::new(
                    location.clone(),
                    if guard.hook
                    {
                        ErrorWhat::HookTimeout
                    }
                    else
                    {
                        ErrorWhat::CleanupTimeout
                    },
                ));
            }
            guard.instructions -= 1;
        }
        Ok(())
    }

    pub(super) fn check_cleanup_return(&self, location: &Location) -> InterpreterResult<()>
    {
        if self
            .exceptions
            .cleanup
            .as_ref()
            .is_some_and(|guard| self.exceptions.frames.len() <= guard.function_depth)
        {
            return Err(InterpreterError::new(
                location.clone(),
                ErrorWhat::CleanupControl,
            ));
        }
        Ok(())
    }

    pub(super) fn execute_try(
        &mut self,
        location: &Location,
        protected: &CompiledTry,
    ) -> InterpreterResult<()>
    {
        self.scope_mut().variables.push_scope();
        let depth = self.scope().variables.current_scope();
        self.exceptions.protected.push(self.exceptions.frames.len());
        let mut result = self.execute_instructions(&protected.body);
        self.exceptions.protected.pop();
        self.scope_mut().variables.reset_to_scope(depth - 1);
        if let Err(mut error) = result
        {
            let uncatchable = error.trace.abort_cleanup
                || matches!(
                    error.what,
                    ErrorWhat::CleanupAbort | ErrorWhat::CleanupTimeout
                );
            let value = self.caught_value(&error);
            if error.trace.raised.is_none()
                && let Some(item) = occurrence(&value)
            {
                error.trace.call_stack = item.borrow().frames.clone();
                item.borrow_mut().status = FailureStatus::Propagating;
                error.trace.resolved.push(item);
                error.trace.raised = Some(value.clone());
            }
            let id = self.scope().types.value_type(value.underlying());
            let selected = if uncatchable
            {
                None
            }
            else
            {
                protected.catches.iter().find(|catch| {
                    catch
                        .type_id
                        .is_none_or(|filter| self.scope().types.catch_matches(filter, id))
                })
            };
            if let Some(catch) = selected
            {
                for occurrence in &error.trace.resolved
                {
                    occurrence.borrow_mut().status = FailureStatus::Acknowledged;
                }
                self.scope_mut().variables.push_scope();
                let depth = self.scope().variables.current_scope();
                if let Some(stack) = &catch.stack
                {
                    let value = self.call_stack_value(&error, id);
                    self.bind_catch(
                        &catch.location,
                        stack,
                        value,
                        Some(self.scope().types.native_types["CallStack"].id),
                    )?;
                }
                let constraint = catch
                    .type_id
                    .map(|id| self.scope().types.catch_type(id))
                    .or_else(|| self.scope().types.builtin_id("ErrorValue"));
                self.bind_catch(&catch.location, &catch.error, value, constraint)?;
                self.exceptions.transfer = None;
                self.exceptions.protected.push(self.exceptions.frames.len());
                result = self.execute_instructions(&catch.code);
                self.exceptions.protected.pop();
                self.scope_mut().variables.reset_to_scope(depth - 1);
                if let Err(handler) = result
                {
                    let original = error
                        .trace
                        .raised
                        .as_ref()
                        .and_then(occurrence)
                        .map(|item| item.borrow().id);
                    result = if handler.trace.rethrow_source.is_some()
                        && handler.trace.rethrow_source == original
                    {
                        Err(handler)
                    }
                    else
                    {
                        self.combine_failure(Err(handler), Err(error), "Handling")
                    };
                }
            }
            else
            {
                result = Err(error);
            }
        }
        if let Some(finally) = &protected.finally
        {
            let skip = result.as_ref().err().is_some_and(|error| {
                error.trace.abort_cleanup
                    || matches!(
                        error.what,
                        ErrorWhat::CleanupAbort | ErrorWhat::CleanupTimeout
                    )
            });
            if !skip
            {
                let inherited = self.exceptions.cleanup.is_some();
                if !inherited
                {
                    let previous = result
                        .as_mut()
                        .err()
                        .and_then(|error| error.trace.cleanup.take())
                        .or_else(|| {
                            self.exceptions
                                .transfer
                                .filter(|_| result.is_ok())
                                .and_then(|_| self.exceptions.transfer_cleanup.take())
                                .filter(|(depth, _)| *depth == self.exceptions.frames.len())
                                .map(|(_, guard)| guard)
                        });
                    self.exceptions.cleanup =
                        Some(previous.unwrap_or_else(|| CleanupGuard {
                            hook: false,
                            deadline: Instant::now() + Duration::from_millis(5000),
                            instructions: 1_000_000,
                            function_depth: self.exceptions.frames.len(),
                            interrupted:
                                result.as_ref().err().is_some_and(|error| {
                                    matches!(error.what, ErrorWhat::Interrupted)
                                }),
                        }));
                    self.exceptions.cleanup.as_mut().unwrap().function_depth =
                        self.exceptions.frames.len();
                }
                let transfer = self.exceptions.transfer.take();
                let returned = self.last_result.take();
                let halted = self.halted;
                self.halted = false;
                self.scope_mut().variables.push_scope();
                let depth = self.scope().variables.current_scope();
                let cleanup = self.execute_instructions(finally);
                self.scope_mut().variables.reset_to_scope(depth - 1);
                let budget = if !inherited
                {
                    self.exceptions.cleanup.take()
                }
                else
                {
                    None
                };
                if result.is_ok()
                    && cleanup.is_err()
                    && transfer == Some(Transfer::Return)
                    && let Some(value) = returned.as_ref().filter(|value| pending(value).is_some())
                {
                    result = Err(self.raise_failure(location, value.clone())?);
                }
                result = self.combine_failure(result, cleanup, "Cleanup");
                if let Err(error) = &mut result
                {
                    error.trace.cleanup = budget;
                }
                else if transfer.is_some()
                    && let Some(budget) = budget
                {
                    self.exceptions.transfer_cleanup = Some((self.exceptions.frames.len(), budget));
                }
                self.last_result = returned;
                self.exceptions.transfer = if result.is_ok() { transfer } else { None };
                self.halted = halted && result.is_ok();
            }
        }
        self.last_result = if self.exceptions.transfer == Some(Transfer::Return)
        {
            self.last_result.take()
        }
        else
        {
            None
        };
        result
    }

    fn bind_catch(
        &mut self,
        location: &Location,
        name: &str,
        value: Value,
        type_id: Option<TypeId>,
    ) -> InterpreterResult<()>
    {
        self.scope_mut()
            .variables
            .create(
                name.into(),
                ScopedValue {
                    value,
                    type_id,
                    exported: ValueVisibility::Private,
                    reference: None,
                    dispatch_type: None,
                    environment_conversion: None,
                },
            )
            .map_err(|message| {
                InterpreterError::new(location.clone(), ErrorWhat::InvalidOperand(message))
            })?;
        self.register_failure_binding(name);
        Ok(())
    }

    pub(super) fn record(&self, name: &str, fields: Vec<Value>) -> Value
    {
        let definition = self
            .scope()
            .types
            .get(self.scope().types.native_types[name].id);
        Value::Struct(Rc::new(StructValue {
            definition,
            fields,
            constructing: false,
            failure: None,
        }))
    }

    fn source_location(&self, location: &Location) -> Value
    {
        self.record(
            "SourceLocation",
            vec![
                Value::from_string(location.origin.to_string()),
                Value::Integer(location.line as i64),
                Value::Integer(location.column as i64),
            ],
        )
    }

    pub(super) fn caught_value(&mut self, error: &InterpreterError) -> Value
    {
        error.trace.raised.clone().unwrap_or_else(|| {
            self.exceptions.next_failure = NEXT_FAILURE.fetch_add(1, Ordering::Relaxed);
            let identity = self.exceptions.next_failure;
            let mut value = self.record(
                "RuntimeError",
                vec![
                    Value::from_string(error.category().into()),
                    Value::from_string(error.what.to_string()),
                ],
            );
            if let Value::Struct(item) = &mut value
            {
                let mut frames = error.trace.call_stack.clone();
                frames.extend(self.exceptions.frames.iter().rev().cloned());
                frames.truncate(128);
                Rc::make_mut(item).failure = Some(Rc::new(RefCell::new(FailureOccurrence {
                    id: identity,
                    owner: 0,
                    control: match error.what
                    {
                        ErrorWhat::Interrupted => Some(ControlFailure::Interrupted),
                        ErrorWhat::CleanupAbort => Some(ControlFailure::CleanupAbort),
                        ErrorWhat::CleanupTimeout => Some(ControlFailure::CleanupTimeout),
                        _ => None,
                    },
                    status: FailureStatus::Acknowledged,
                    rethrow_source: None,
                    origin: error.location.clone(),
                    native_origin: error.trace.native_location.clone(),
                    frames,
                    propagation: Vec::new(),
                    related: error.trace.related.clone(),
                    slots: HashSet::new(),
                    formatter: {
                        let string = self.scope().types.builtin_id("String").unwrap();
                        let name = self
                            .scope()
                            .types
                            .conversion_method_name(item.definition.id);
                        let typed = conversion_key(string, &name, item.definition.id);
                        let plain = method_key(string, &name);
                        self.scope()
                            .lexical_function(&typed)
                            .or_else(|| self.scope().imported_methods.get(&typed).cloned())
                            .or_else(|| self.scope().lexical_function(&plain))
                            .or_else(|| self.scope().imported_methods.get(&plain).cloned())
                    },
                })));
            }
            value
        })
    }

    pub(super) fn call_stack_value(&self, error: &InterpreterError, id: TypeId) -> Value
    {
        let active: &[CallFrame] = if error.trace.raised.is_some()
        {
            &[]
        }
        else
        {
            &self.exceptions.frames
        };
        let frames = error
            .trace
            .call_stack
            .iter()
            .chain(active.iter().rev())
            .take(128)
            .map(|frame| {
                self.record(
                    "CallFrame",
                    vec![
                        Value::from_string(frame.name.clone()),
                        self.source_location(&frame.location),
                        Value::Boolean(frame.native),
                        frame
                            .native_location
                            .as_ref()
                            .map(|location| self.source_location(location))
                            .unwrap_or(Value::None),
                    ],
                )
            })
            .collect();
        let propagation = error
            .trace
            .raised
            .as_ref()
            .and_then(occurrence)
            .map(|item| {
                item.borrow()
                    .propagation
                    .iter()
                    .take(128)
                    .map(|location| self.source_location(location))
                    .collect()
            })
            .unwrap_or_default();
        let related = error
            .trace
            .related
            .iter()
            .take(16)
            .map(|related| {
                let frames = related
                    .frames
                    .iter()
                    .take(128)
                    .map(|frame| {
                        self.record(
                            "CallFrame",
                            vec![
                                Value::from_string(frame.name.clone()),
                                self.source_location(&frame.location),
                                Value::Boolean(frame.native),
                                frame
                                    .native_location
                                    .as_ref()
                                    .map(|location| self.source_location(location))
                                    .unwrap_or(Value::None),
                            ],
                        )
                    })
                    .collect();
                let ty = field(&related.snapshot, "original_type").unwrap_or(Value::None);
                let stack = self.record(
                    "CallStack",
                    vec![
                        ty,
                        self.source_location(&related.origin),
                        related
                            .native_origin
                            .as_ref()
                            .map(|location| self.source_location(location))
                            .unwrap_or(Value::None),
                        Value::from_array(frames),
                        Value::from_array(Vec::new()),
                        Value::from_array(Vec::new()),
                        Value::Boolean(false),
                    ],
                );
                self.record(
                    "RelatedFailure",
                    vec![
                        Value::from_string(related.relation.clone()),
                        related.snapshot.clone(),
                        stack,
                    ],
                )
            })
            .collect();
        self.record(
            "CallStack",
            vec![
                Value::Type(self.scope().types.get(id)),
                self.source_location(&error.location),
                error
                    .trace
                    .native_location
                    .as_ref()
                    .map(|location| self.source_location(location))
                    .unwrap_or(Value::None),
                Value::from_array(frames),
                Value::from_array(propagation),
                Value::from_array(related),
                Value::Boolean(false),
            ],
        )
    }

    pub(super) fn related_failure(
        &mut self,
        error: &InterpreterError,
        relation: &str,
    ) -> RelatedFailure
    {
        let value = self.caught_value(error);
        let id = self.scope().types.value_type(value.underlying());
        RelatedFailure {
            relation: relation.into(),
            origin: error.location.clone(),
            native_origin: error.trace.native_location.clone(),
            frames: error.trace.call_stack.iter().take(128).cloned().collect(),
            snapshot: self.record(
                "ErrorSnapshot",
                vec![
                    Value::Type(self.scope().types.get(id)),
                    Value::None,
                    Value::from_string(error.what.to_string().chars().take(4096).collect()),
                    Value::Boolean(true),
                ],
            ),
        }
    }
}

pub(super) fn field(value: &Value, name: &str) -> Option<Value>
{
    let Value::Struct(item) = value.underlying()
    else
    {
        return None;
    };
    let TypeKind::Struct(fields) = &item.definition.kind
    else
    {
        return None;
    };
    fields
        .iter()
        .position(|field| field.name == name)
        .and_then(|index| item.fields.get(index).cloned())
}

pub(super) fn public_summary(value: &Value) -> String
{
    let Value::Struct(item) = value.underlying()
    else
    {
        return value.type_name();
    };
    let TypeKind::Struct(fields) = &item.definition.kind
    else
    {
        return value.type_name();
    };
    let mut text: String = item.definition.name.chars().take(256).collect();
    text.push_str(" {");
    for (index, (field, value)) in fields
        .iter()
        .zip(&item.fields)
        .filter(|(field, _)| field.public)
        .take(16)
        .enumerate()
    {
        if index > 0
        {
            text.push_str(", ");
        }
        text.extend(field.name.chars().take(128));
        text.push_str(": ");
        match value.underlying()
        {
            Value::String(value, _) =>
            {
                text.extend(value.chars().take(512));
            }
            Value::Integer(_) | Value::Float(_, _) | Value::Boolean(_) | Value::None =>
            {
                text.push_str(&value.as_text())
            }
            _ =>
            {
                text.push('<');
                text.extend(value.type_name().chars().take(128));
                text.push('>');
            }
        }
    }
    text.push('}');
    text
}
