
mod stream;
#[cfg(unix)]
mod process;
mod conversion;
mod graph;

use std::{ rc::Rc, thread::sleep, time::Duration };

use crate::{ language::{ bytecode::CompiledPipeline,
                         data::value::{ ExecResult, Value },
                         interpreter::{ Interpreter, InterpreterResult, InterpreterError, ErrorWhat,
                                        pipeline::{ stream::StageStream,
                                                    conversion::BoundaryStream } },
                         text::location::Location },
             runtime::signals::take_interrupt };


#[derive(Clone)]
pub(super) enum Data
{
    Value(Value),
    Bytes(Vec<u8>),
}


pub(super) enum Step
{
    Pending,
    Data(Data),
    End,
}


pub(super) struct Context
{
    statuses: Vec<Option<ExecResult>>,
}


impl Context
{
    fn new(count: usize) -> Self { Self { statuses: vec![None; count] } }

    pub fn record(&mut self, stage: usize, result: ExecResult)
    {
        let status = &mut self.statuses[stage];
        if status.is_none() || matches!(status, Some(ExecResult::Value(0)))
        { *status = Some(result); }
    }

    pub fn failed(&self) -> bool
    {
        self.statuses.iter().flatten().any(|status| !matches!(status, ExecResult::Value(0)))
    }

    fn status(&self) -> Option<ExecResult>
    {
        self.statuses.iter().flatten().find(|status| !matches!(status, ExecResult::Value(0)))
            .or_else(|| self.statuses.iter().flatten().next()).cloned()
    }
}


pub(super) trait Stream
{
    fn poll(&mut self, interpreter: &mut Interpreter, context: &mut Context)
        -> InterpreterResult<Step>;
    fn cancel(&mut self, interpreter: &mut Interpreter, context: &mut Context)
        -> InterpreterResult<()>;
    fn bytes_output(&self) -> bool;
}


impl Interpreter
{
    pub(super) fn execute_pipeline(&mut self, pipeline: &Rc<CompiledPipeline>)
    -> InterpreterResult<()>
    {
        self.pipeline_depth += 1;
        let result = self.execute_pipeline_inner(pipeline);
        self.pipeline_depth -= 1;
        result
    }

    fn execute_pipeline_inner(&mut self, pipeline: &Rc<CompiledPipeline>)
    -> InterpreterResult<()>
    {
        if pipeline.primary_len != pipeline.stages.len()
        { return self.execute_split_pipeline(pipeline); }
        // Each layer pulls at most one item/chunk. External descriptors use nonblocking
        // reads/writes, so upstream and downstream processes can progress concurrently.
        let depth = self.pipeline_inputs.len();
        let mut context = Context::new(pipeline.stages.len());
        let first = &pipeline.boundaries[0];
        let iterate_source = !first.annotation.as_ref().is_some_and(|format|
            self.pipeline_format_supported(first, format, true));
        let mut stream: Box<dyn Stream + '_> =
            Box::new(StageStream::new(&pipeline.stages[0], 0, None, iterate_source));
        for (index, stage) in pipeline.stages.iter().enumerate().skip(1)
        {
            let boundary = &pipeline.boundaries[index - 1];
            let input = BoundaryStream::new(self, boundary, stream)?;
            stream = Box::new(StageStream::new(stage, index, Some(Box::new(input)), false));
        }
        let result = (|| {
                let mut value = Value::None;
                loop
            {
                    if self.halted { stream.cancel(self, &mut context)?; break; }
                if take_interrupt()
                    { return Err(InterpreterError::new(pipeline.stages[0].location.clone(),
                        ErrorWhat::Interrupted)); }
                let step = stream.poll(self, &mut context)?;
                match step
                    {
                        Step::Pending =>
                            {
                                if crate::runtime::tasks::active()
                                { crate::runtime::tasks::cooperate().map_err(|error|
                                    InterpreterError::new(pipeline.stages[0].location.clone(),
                                        match error.kind.as_str()
                                        {
                                            "Interrupted" => ErrorWhat::Interrupted,
                                            "CleanupAbort" => ErrorWhat::CleanupAbort,
                                            _ => ErrorWhat::UnsupportedSuspension(error.message),
                                        }))?; }
                                else { sleep(Duration::from_millis(1)); }
                            },
                        Step::End => break,
                        Step::Data(Data::Value(item)) =>
                            {
                                self.consume_success(&pipeline.stages.last().unwrap().location,
                                                     &item)?;
                                value = item;
                            },
                        Step::Data(Data::Bytes(bytes)) =>
                            {
                                let location = &pipeline.stages.last().unwrap().location;
                                self.pipeline_stdout(location, &bytes)?;
                            },
                    }
                }
                self.last_result = Some(context.status().map(Value::ExecResult).unwrap_or(value));
                Ok(())
        })();
        let result = if result.is_err()
        {
            let cleanup = stream.cancel(self, &mut context);
            self.combine_failure(result, cleanup, "Cleanup")
        }
        else
        {
            result
        };
        self.pipeline_inputs.truncate(depth);
        result
    }

    pub(super) fn pipeline_stdout(&mut self, location: &Location, bytes: &[u8])
        -> InterpreterResult<()>
    {
        let result = if let Some(output) = self.redirections.iter().rev()
            .find_map(|redirect| redirect.output.as_ref())
            { output.copy_from(&mut &*bytes).map(|_| ()) }
            else if let Some(capture) = self.captured_stdout.as_mut()
            { capture.extend_from_slice(bytes); Ok(()) }
            else { crate::runtime::readline::output::write_data(bytes) };
        result.map_err(|error| Self::redirection_error(location, error))
    }
}
