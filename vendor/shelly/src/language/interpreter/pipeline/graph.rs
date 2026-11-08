
use std::{ cell::RefCell, collections::{ HashSet, VecDeque }, rc::Rc,
           sync::{ Arc, atomic::{ AtomicBool, Ordering } } };

use crate::{ language::{ bytecode::CompiledPipeline,
                         data::{ retained::Budget, value::Value },
                         interpreter::{ Interpreter, InterpreterError, InterpreterResult, ErrorWhat,
                             pipeline::{ Context, Data, Step, Stream,
                                 stream::StageStream, conversion::BoundaryStream } },
                         text::location::Location },
             runtime::{ signals::take_interrupt, tasks } };

const QUEUED_BYTES: usize = 32 * 1024 * 1024;


struct Port
{
    queue: VecDeque<(Data, usize)>,
    active: bool,
    ended: bool,
    bytes: bool,
}


struct Graph
{
    ports: Vec<Port>,
    retained: usize,
    context: Context,
    finished: usize,
    error: Option<InterpreterError>,
    value: Value,
    output: Vec<u8>,
    halt: Option<u8>,
}


// Reactor cancellation can unwind a suspended stack. Completion must still be
// published so the coordinator cannot wait forever for an abandoned worker.
struct Completion
{
    graph: Rc<RefCell<Graph>>,
    outputs: Vec<usize>,
    finished: bool,
}


impl Drop for Completion
{
    fn drop(&mut self)
    {
        if self.finished { return; }
        let mut graph = self.graph.borrow_mut();
        for port in &self.outputs { graph.ports[*port].ended = true; }
        graph.finished += 1;
    }
}


struct Input
{
    graph: Rc<RefCell<Graph>>,
    port: usize,
}


impl Stream for Input
{
    fn poll(&mut self, _interpreter: &mut Interpreter, _context: &mut Context)
        -> InterpreterResult<Step>
    {
        let mut graph = self.graph.borrow_mut();
        if let Some((data, size)) = graph.ports[self.port].queue.pop_front()
        {
            graph.retained -= size;
            return Ok(Step::Data(data));
        }
        Ok(if graph.ports[self.port].ended { Step::End } else { Step::Pending })
    }

    fn cancel(&mut self, _interpreter: &mut Interpreter, _context: &mut Context)
        -> InterpreterResult<()>
    {
        let mut graph = self.graph.borrow_mut();
        graph.ports[self.port].active = false;
        let released: usize = graph.ports[self.port].queue.drain(..).map(|(_, size)| size).sum();
        graph.retained -= released;
        Ok(())
    }

    fn bytes_output(&self) -> bool { self.graph.borrow().ports[self.port].bytes }
}


fn interrupted(location: &Location, fault: tasks::NativeFault) -> InterpreterError
{
    InterpreterError::new(location.clone(), match fault.kind.as_str()
        {
            "Interrupted" => ErrorWhat::Interrupted,
            "CleanupAbort" => ErrorWhat::CleanupAbort,
            _ => ErrorWhat::UnsupportedSuspension(fault.message),
        })
}


fn cooperate(location: &Location) -> InterpreterResult<()>
{ tasks::cooperate().map_err(|fault| interrupted(location, fault)) }


fn retained(location: &Location, data: &Data) -> InterpreterResult<usize>
{
    match data
    {
        Data::Bytes(bytes) => Ok(bytes.len()),
        Data::Value(value) =>
            {
                let mut budget = Budget::default();
                budget.value(value).map_err(|message| InterpreterError::new(location.clone(),
                    ErrorWhat::InvalidOperand(format!("Pipeline broadcast: {}. \
                        Adapt live resources to bytes or items before splitting", message))))?;
                Ok(budget.used_bytes())
            },
    }
}


impl Interpreter
{
    pub(super) fn execute_split_pipeline(&mut self, pipeline: &Rc<CompiledPipeline>)
        -> InterpreterResult<()>
    {
        let location = &pipeline.stages[0].location;
        let invalid = |message| InterpreterError::new(location.clone(),
            ErrorWhat::InvalidOperand(message));
        if self.exceptions.cleanup.is_some() || self.exceptions.reporting
        { return Err(invalid("Split pipelines are not admitted during cleanup/reporting".into())); }
        let count = pipeline.stages.len();
        let mut children = vec![Vec::new(); count];
        for (edge, parent) in pipeline.parents.iter().enumerate()
        { children[*parent].push(edge + 1); }
        let mut destinations = HashSet::new();
        for stage in &pipeline.stages
        {
            if let Some(name) = &stage.destination
                && let Some(binding) = self.variable_binding(name)
                && !destinations.insert(Rc::as_ptr(&binding) as usize)
            { return Err(invalid(format!("Duplicate pipeline destination '{}'", name))); }
        }
        let source_modes: Vec<_> = children[0].iter().map(|child|
            {
                let boundary = &pipeline.boundaries[child - 1];
                boundary.annotation.as_ref().is_some_and(|format|
                    self.pipeline_format_supported(boundary, format, true))
            }).collect();
        if source_modes.iter().any(|mode| *mode != source_modes[0])
        { return Err(invalid("Split source mixes whole-value encoding and iteration; \
            put an explicit stage before the fork".into())); }
        let iterate = !source_modes[0];
        // Maximal straight segments preserve the existing stage adapters. Only
        // forks introduce workers, so ordinary pipelines keep their fast path.
        let mut segments = Vec::new();
        let mut starts = vec![0];
        while let Some(start) = starts.pop()
        {
            let mut segment = vec![start];
            let mut end = start;
            while children[end].len() == 1
            {
                end = children[end][0];
                segment.push(end);
            }
            starts.extend(children[end].iter().rev());
            segments.push(segment);
        }
        let reactor = tasks::current_reactor().ok_or_else(||
            invalid("Split pipeline requires the cooperative I/O reactor".into()))?;
        if segments.len() > reactor.available_tasks()
        { return Err(invalid("Split pipeline exceeds the available VM task limit".into())); }
        let graph = Rc::new(RefCell::new(Graph
            {
                ports: (0..count).map(|_| Port { queue: VecDeque::new(), active: true,
                    ended: false, bytes: false }).collect(),
                retained: 0, context: Context::new(count), finished: 0, error: None,
                value: Value::None, output: Vec::new(), halt: None,
            }));
        // Validate edge codecs before any worker can execute a stage.
        for (edge, boundary) in pipeline.boundaries.iter().enumerate()
        {
            let input = Input { graph: graph.clone(), port: edge + 1 };
            BoundaryStream::new(self, boundary, Box::new(input))?;
        }
        let plan = pipeline.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let workers = segments.len();
        for segment in segments
        {
            let mut vm = self.fork_task();
            vm.pipeline_worker = true;
            vm.pipeline_inputs = self.pipeline_inputs.clone();
            vm.exceptions.frames = self.exceptions.frames.clone();
            vm.exceptions.callers = self.exceptions.callers.clone();
            vm.redirections = self.redirections.iter().map(|item| item.borrowed()).collect();
            vm.captured_stdout = self.captured_stdout.as_ref().map(|_| Vec::new());
            let plan = plan.clone();
            let graph = graph.clone();
            let canceled = cancel.clone();
            let outputs = children[*segment.last().unwrap()].clone();
            if let Err(error) = reactor.spawn(canceled.clone(), move ||
                {
                    let mut completion = Completion
                        { graph: graph.clone(), outputs: outputs.clone(), finished: false };
                    let first = segment[0];
                    let last = *segment.last().unwrap();
                    let stage_location = plan.stages[first].location.clone();
                    let mut context = Context::new(count);
                    let region = vm.enter_failure_region();
                    let result = (||
                        {
                            let input: Option<Box<dyn Stream>> = if first == 0 { None }
                                else
                                {
                                    Some(Box::new(BoundaryStream::new(&vm,
                                        &plan.boundaries[first - 1], Box::new(Input
                                            { graph: graph.clone(), port: first }))?))
                                };
                            let mut stream: Box<dyn Stream> = Box::new(StageStream::new(
                                &plan.stages[first], first, input, iterate));
                            for index in segment.iter().skip(1)
                            {
                                let input = BoundaryStream::new(&vm,
                                    &plan.boundaries[index - 1], stream)?;
                                stream = Box::new(StageStream::new(&plan.stages[*index], *index,
                                    Some(Box::new(input)), false));
                            }
                            let operation = (||
                                {
                                    loop
                                    {
                                        if canceled.load(Ordering::Acquire) { break; }
                                        if !outputs.is_empty() && outputs.iter().all(|port|
                                            !graph.borrow().ports[*port].active)
                                        { break; }
                                        if outputs.iter().any(|port|
                                            {
                                                let state = graph.borrow();
                                                state.ports[*port].active &&
                                                    !state.ports[*port].queue.is_empty()
                                            })
                                        { cooperate(&stage_location)?; continue; }
                                        let step = stream.poll(&mut vm, &mut context)?;
                                        {
                                            let mut state = graph.borrow_mut();
                                            for port in &outputs
                                            { state.ports[*port].bytes = stream.bytes_output(); }
                                            for (stage, status) in
                                                context.statuses.iter().enumerate()
                                            {
                                                if let Some(status) = status
                                                { state.context.record(stage, status.clone()); }
                                            }
                                            if state.context.failed()
                                            { canceled.store(true, Ordering::Release); }
                                        }
                                        match step
                                        {
                                            Step::End => break,
                                            Step::Pending => {},
                                            Step::Data(data) if outputs.is_empty() =>
                                                {
                                                    match data
                                                    {
                                                        Data::Value(value) =>
                                                            {
                                                                vm.consume_success(
                                                                    &plan.stages[last].location,
                                                                    &value)?;
                                                                if last == plan.primary_len - 1
                                                                {
                                                                    graph.borrow_mut().value =
                                                                        value;
                                                                }
                                                            },
                                                        Data::Bytes(bytes) => vm.pipeline_stdout(
                                                            &plan.stages[last].location, &bytes)?,
                                                    }
                                                },
                                            Step::Data(data) =>
                                                {
                                                    let size = retained(&stage_location, &data)?;
                                                    let copies = outputs.iter().filter(|port|
                                                        graph.borrow().ports[**port].active)
                                                        .count();
                                                    let total = size.saturating_mul(copies);
                                                    if total > QUEUED_BYTES
                                                    { return Err(InterpreterError::new(
                                                        stage_location.clone(),
                                                        ErrorWhat::InvalidOperand(
                                                            "Pipeline broadcast item exceeds \
                                                             retained-byte limit".into()))); }
                                                    while graph.borrow().retained + total
                                                        > QUEUED_BYTES
                                                    { cooperate(&stage_location)?; }
                                                    let mut state = graph.borrow_mut();
                                                    for port in &outputs
                                                    {
                                                        if state.ports[*port].active
                                                        {
                                                            state.ports[*port].queue
                                                                .push_back((data.clone(), size));
                                                            state.retained += size;
                                                        }
                                                    }
                                                },
                                        }
                                        if vm.halted
                                        {
                                            graph.borrow_mut().halt = Some(vm.exit_code);
                                            canceled.store(true, Ordering::Release);
                                            break;
                                        }
                                        cooperate(&stage_location)?;
                                    }
                                    Ok(())
                                })();
                            for (stage, status) in
                                graph.borrow().context.statuses.iter().enumerate()
                            {
                                if let Some(status) = status
                                { context.record(stage, status.clone()); }
                            }
                            let cleanup = stream.cancel(&mut vm, &mut context);
                            vm.combine_failure(operation, cleanup, "Pipeline cleanup")
                        })();
                    let result = vm.leave_failure_region(region, result, false);
                    let mut state = graph.borrow_mut();
                    for (stage, status) in context.statuses.iter().enumerate()
                    {
                        if let Some(status) = status
                        { state.context.record(stage, status.clone()); }
                    }
                    if let Err(error) = result
                    {
                        if state.error.is_none() && !canceled.load(Ordering::Acquire)
                        { state.error = Some(error); }
                        canceled.store(true, Ordering::Release);
                    }
                    if let Some(bytes) = vm.captured_stdout.take() { state.output.extend(bytes); }
                    for port in &outputs { state.ports[*port].ended = true; }
                    state.finished += 1;
                    completion.finished = true;
                })
            {
                cancel.store(true, Ordering::Release);
                return Err(invalid(format!("Cannot allocate pipeline worker: {}", error)));
            }
        }
        let mut interruption = None;
        while graph.borrow().finished < workers
        {
            if take_interrupt() && interruption.is_none()
            {
                interruption = Some(InterpreterError::new(location.clone(),
                    ErrorWhat::Interrupted));
                cancel.store(true, Ordering::Release);
            }
            if let Err(error) = cooperate(location)
            {
                if interruption.is_none() { interruption = Some(error); }
                cancel.store(true, Ordering::Release);
            }
        }
        let mut state = graph.borrow_mut();
        if let Some(bytes) = self.captured_stdout.as_mut() { bytes.append(&mut state.output); }
        if let Some(error) = interruption.or_else(|| state.error.take()) { return Err(error); }
        if let Some(code) = state.halt { self.halted = true; self.exit_code = code; }
        self.last_result = Some(state.context.status().map(Value::ExecResult)
            .unwrap_or_else(|| state.value.clone()));
        Ok(())
    }
}
