//! File-backed mount acquisition and acknowledged backing-source retirement.
#![forbid(unsafe_code)]
use super::*;
use crate::drivers::file_sources::runtime as backing;

pub(super) enum Acquire
{
    Catalog
    {
        handle: u32, provider: u64
    },
    Open
    {
        handle: u32, description: Vec<u8>
    },
    Cleanup
    {
        handle: u32, status: Status
    }
}
#[derive(Clone)]
struct Context
{
    subject:   IoSubject,
    owner:     super::super::super::subject::Subject,
    thread:    u64,
    key:       Key,
    provider:  u64,
    abandoned: bool
}
impl Context
{
    fn select(&mut self, description: &[u8]) -> Result<bool, Status>
    {
        let m = fs::Metadata::decode(description).map_err(|_| BAD_ABI)?;
        let kind = m.finder_info[..4].try_into().map_err(|_| BAD_ABI)?;
        let Some(provider) = crate::drivers::file_handler_provider(kind, m.name)?
        else
        {
            return Ok(false);
        };
        self.provider = provider;
        Ok(true)
    }

    fn existing(&self) -> Result<Option<VolumeInfo>, Status>
    {
        match backing::find(self.provider, self.key.volume, self.key.cnid)?
        {
            None => Ok(None),
            Some(source) =>
            {
                files(|m| {
                    m.existing_provider_mount(self.provider, super::super::Source::File(source))
                })?.map(Some)
                   .ok_or(SERVICE_BUSY)
            }
        }
    }

    fn from(flight: &Flight) -> Self
    {
        Self { subject:   flight.job.subject().clone(),
               owner:     flight.owner.clone(),
               thread:    flight.thread,
               key:       flight.key,
               provider:  flight.provider,
               abandoned: flight.abandoned }
    }

    fn flight(self, step: Step, job: OwnedJob) -> Flight
    {
        Flight { owner: self.owner,
                 thread: self.thread,
                 key: self.key,
                 provider: self.provider,
                 abandoned: self.abandoned,
                 step,
                 job,
                 input: String::new(),
                 path: None }
    }

    fn release(self, source: u32, volume: Option<u32>, status: Status) -> Progress
    {
        match backing::begin_release(self.provider, source)
        {
            Ok(None) => retired(volume, status),
            Ok(Some((closing, job))) =>
            {
                Progress::Pending(self.flight(Step::Release { volume,
                                                              closing,
                                                              status },
                                              job))
            }
            Err(error) =>
            {
                if let Some(v) = volume
                {
                    let _ = files(|m| m.quarantine_provider_mount(v));
                }
                Progress::Done(Err(error))
            }
        }
    }

    fn mount(self, source: u32, description: &[u8]) -> Progress
    {
        let volume = match files(|manager| {
                  manager.reserve_provider_mount(self.provider, super::super::Source::File(source))
              })
        {
            Ok(volume) => volume,
            Err(error) => return self.release(source, None, error)
        };
        match submit_args(&self.subject,
                          self.provider,
                          Operation::Mount,
                          Arguments { volume,
                                      auxiliary: source.into(),
                                      payload: description,
                                      ..Default::default() })
        {
            Ok(job) => Progress::Pending(self.flight(Step::Mount(volume), job)),
            Err(error) => self.release(source, Some(volume), error)
        }
    }

    fn cleanup(self, handle: u32, status: Status) -> Progress
    {
        let job = files(|m| m.close_provider_source(handle, true)).and_then(|(provider, args)| {
                                                                      submit_args(&self.subject,
                                                                                  provider,
                                                                                  Operation::Close,
                                                                                  args)
                                                                  });
        match job
        {
            Ok(job) =>
            {
                Progress::Pending(self.flight(Step::Source(Acquire::Cleanup { handle,
                                                                              status }),
                                              job))
            }
            Err(_) =>
            {
                let _ = files(|m| m.quarantine_provider_source(handle));
                Progress::Done(Err(ROLLBACK_FAILED))
            }
        }
    }
}
pub(super) fn release(flight: Flight, source: u32, volume: Option<u32>, status: Status)
                      -> Progress
{
    Context::from(&flight).release(source, volume, status)
}

pub(super) fn start_file(owner: u64, thread: u64, key: Key, call: &mut FileCall, output: &mut [u8])
                         -> Status
{
    let subject = match io_subject(owner.into())
    {
        Ok(subject) => subject,
        Err(error) => return error
    };
    let mut ctx = Context { subject,
                            owner: owner.into(),
                            thread,
                            key,
                            provider: 0,
                            abandoned: false };
    let progress = match files(|m| m.source_provider(call.volume))
    {
        Ok(None) =>
        {
            let description = match files(|m| m.describe_block_source(call.volume, call.cnid))
            {
                Ok(bytes) => bytes,
                Err(error) => return error
            };
            match ctx.select(&description)
            {
                Ok(true) => (),
                Ok(false) => return reply(call, output, Ok(Reply::NoHandler)),
                Err(error) => return error
            }
            match ctx.existing()
            {
                Ok(Some(info)) => return reply(call, output, Ok(Reply::Mounted(info))),
                Ok(None) => (),
                Err(error) => return error
            }
            let source = match backing::lease(ctx.provider, call.volume, call.cnid)
            {
                Ok((source, true)) => source,
                Ok((_, false)) => return SERVICE_BUSY,
                Err(error) => return error
            };
            ctx.mount(source, &description)
        }
        Ok(Some(source_provider)) =>
        {
            let (actual, handle) =
                match files(|m| m.reserve_provider_source(call.volume, call.cnid))
                {
                    Ok(mark) => mark,
                    Err(error) => return error
                };
            if actual != source_provider
            {
                let _ = files(|m| m.retire_provider_source(handle));
                return BAD_ABI;
            }
            match submit_args(&ctx.subject, source_provider,
                              Operation::Catalog,
                              Arguments { volume: call.volume,
                                          node: call.cnid,
                                          ..Default::default() })
            {
                Ok(job) => Progress::Pending(ctx.flight(Step::Source(Acquire::Catalog {
                                                        handle,
                                                        provider: source_provider
                                                    }), job)),
                Err(error) =>
                {
                    Progress::Done(files(|m| m.retire_provider_source(handle)).and(Err(error)))
                }
            }
        }
        Err(error) => return error
    };
    match progress
    {
        Progress::Done(result) => reply(call, output, result),
        Progress::Pending(flight) => waiting(flight)
    }
}
pub(super) fn advance(flight: Flight, result: Result<Vec<u8>, Status>, never_sent: bool)
                      -> Progress
{
    let mut ctx = Context::from(&flight);
    let Step::Source(step) = flight.step
    else
    {
        return Progress::Done(Err(BAD_ABI));
    };
    match step
    {
        Acquire::Catalog { handle,
                           provider } =>
        {
            let description = result.and_then(|bytes| {
                                        if ctx.abandoned
                                        {
                                            return Err(OPERATION_CANCELLED);
                                        }
                                        let meta =
                                            fs::Metadata::decode(&bytes).map_err(|_| BAD_ABI)?;
                                        if meta.directory || meta.id != ctx.key.cnid
                                        {
                                            return Err(BAD_ARGUMENT);
                                        }
                                        Ok(bytes)
                                    });
            let description =
                match description
                {
                    Ok(bytes) => bytes,
                    Err(error) => return Progress::Done(files(|m| {
                                                            m.retire_provider_source(handle)
                                                        }).and(Err(error)))
                };
            let selected = ctx.select(&description).and_then(|selected| {
                                                       if !selected
                                                       {
                                                           return Ok(Some(Reply::NoHandler));
                                                       }
                                                       // A serialized provider cannot serve its
                                                       // own backing reads.
                                                       if ctx.provider == provider
                                               && crate::drivers::provider_serialized(provider)?
                                            {
                                                return Err(NOT_SUPPORTED);
                                            }
                                                       ctx.existing().map(|v| v.map(Reply::Mounted))
                                                   });
            match selected
            {
                Ok(None) => (),
                terminal =>
                {
                    let retired = files(|m| m.retire_provider_source(handle));
                    return Progress::Done(retired.and(terminal).and_then(|r| r.ok_or(BAD_ABI)));
                }
            }
            match submit_args(&ctx.subject,
                              provider,
                              Operation::Open,
                              Arguments { volume: ctx.key.volume,
                                          node: ctx.key.cnid,
                                          token: u64::from(handle),
                                          ..Default::default() })
            {
                Ok(job) =>
                {
                    Progress::Pending(ctx.flight(Step::Source(Acquire::Open { handle,
                                                                              description }),
                                                 job))
                }
                Err(error) =>
                {
                    Progress::Done(files(|m| m.retire_provider_source(handle)).and(Err(error)))
                }
            }
        }
        Acquire::Open { handle,
                        description } =>
        {
            if never_sent
            {
                return Progress::Done(files(|m| m.retire_provider_source(handle))
                                          .and(Err(OPERATION_CANCELLED)));
            }
            let opened =
                result.and_then(|bytes| {
                          if ctx.abandoned
                          {
                              return Err(OPERATION_CANCELLED);
                          }
                          let length = files(|m| m.publish_provider_source(handle, &bytes))?;
                          if fs::Metadata::decode(&description).map_err(|_| BAD_ABI)?.data_length
                             != length
                          {
                              return Err(SCAN_STALE);
                          }
                          backing::adopt(ctx.provider, ctx.key.volume, ctx.key.cnid, handle, length)
                      });
            match opened
            {
                Ok(source) => ctx.mount(source, &description),
                Err(error) => ctx.cleanup(handle, error)
            }
        }
        Acquire::Cleanup { handle,
                           status } =>
        {
            if matches!(&result,Ok(bytes) if bytes.is_empty()) || matches!(result, Err(NOT_FOUND))
            {
                Progress::Done(files(|m| m.retire_provider_source(handle)).and(Err(status)))
            }
            else
            {
                let _ = files(|m| m.quarantine_provider_source(handle));
                Progress::Done(Err(ROLLBACK_FAILED))
            }
        }
    }
}
