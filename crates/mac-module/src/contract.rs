use crate::*;
fn bytes<const N: usize>(data: &[u8], at: usize) -> Result<[u8; N]>
{
    data.get(at..at.checked_add(N).ok_or(Error::Malformed)?)
        .ok_or(Error::Malformed)?
        .try_into()
        .map_err(|_| Error::Malformed)
}
fn u16_at(data: &[u8], at: usize) -> Result<u16>
{
    Ok(u16::from_le_bytes(bytes(data, at)?))
}
fn u32_at(data: &[u8], at: usize) -> Result<u32>
{
    Ok(u32::from_le_bytes(bytes(data, at)?))
}
fn u64_at(data: &[u8], at: usize) -> Result<u64>
{
    Ok(u64::from_le_bytes(bytes(data, at)?))
}
fn text(data: &[u8]) -> Result<&str>
{
    let value = core::str::from_utf8(data).map_err(|_| Error::Malformed)?;
    if value.chars().any(char::is_control)
    {
        return Err(Error::Malformed);
    }
    Ok(value)
}
#[derive(Clone, Copy, Debug)]
pub struct Contract<'a>
{
    pub kind:        PackageKind,
    pub kernel_abi:  u32,
    pub broker_abi:  u32,
    pub target:      Target,
    pub identity:    [u8; 16],
    pub code_type:   [u8; 4],
    pub code_id:     i16,
    pub code_digest: [u8; 32],
    pub heap_bytes:  u32,
    pub stack_bytes: u32,
    pub name:        &'a str,
    pub version:     &'a str,
    pub provenance:  &'a str,
    offers:          &'a [u8],
    dependencies:    &'a [u8],
    services:        &'a [u8],
    devices:         &'a [u8]
}
impl<'a> Contract<'a>
{
    pub fn decode(data: &'a [u8]) -> Result<Self>
    {
        if data.len() < HEADER_BYTES || data.len() > MAX_BYTES
        {
            return Err(Error::Limit);
        }
        if bytes::<4>(data, 0)? != *b"KMOD"
        {
            return Err(Error::Malformed);
        }
        if u16_at(data, 4)? != SCHEMA
        {
            return Err(Error::Version);
        }
        if u16_at(data, 6)? as usize != HEADER_BYTES
           || u32_at(data, 8)? as usize != data.len()
           || data[70..72] != [0; 2]
           || data[86..88] != [0; 2]
           || u64_at(data, 120)? != abi::LAYOUT_ID
        {
            return Err(Error::Malformed);
        }
        let kind = match bytes::<4>(data, 12)?
        {
            v if v == *b"DRVR" => PackageKind::Driver,
            v if v == *b"INIT" => PackageKind::Extension,
            _ => return Err(Error::Contract)
        };
        let target = Target { architecture: u16_at(data, 24)?,
                              pointer_bits: data[26],
                              endian:       data[27],
                              convention:   u32_at(data, 28)?,
                              features:     u64_at(data, 32)? };
        if !matches!(target.pointer_bits, 32 | 64)
           || !matches!(target.endian, Target::LITTLE | Target::BIG)
           || target.convention != Target::C_ABI
        {
            return Err(Error::Target);
        }
        let identity = bytes(data, 40)?;
        let code_type = bytes(data, 56)?;
        let code_id = i16::from_le_bytes(bytes(data, 60)?);
        if identity == [0; 16]
           || code_type != kind.code_type()
           || kind == PackageKind::Extension && ExtensionTarget::from_resource_id(code_id).is_none()
        {
            return Err(Error::Contract);
        }
        let mut cursor = HEADER_BYTES;
        let mut take = |count_at: usize, size: usize| -> Result<&'a [u8]> {
            let count = usize::from(u16_at(data, count_at)?);
            if count > MAX_RECORDS
            {
                return Err(Error::Limit);
            }
            let end = cursor.checked_add(count.checked_mul(size).ok_or(Error::Limit)?)
                            .ok_or(Error::Limit)?;
            let part = data.get(cursor..end).ok_or(Error::Malformed)?;
            cursor = end;
            Ok(part)
        };
        let offers = take(62, OFFER_BYTES)?;
        let dependencies = take(64, DEPENDENCY_BYTES)?;
        let services = take(66, SERVICE_BYTES)?;
        let devices = take(68, MATCH_BYTES)?;
        let name_end = cursor.checked_add(usize::from(u16_at(data, 80)?)).ok_or(Error::Limit)?;
        let version_end =
            name_end.checked_add(usize::from(u16_at(data, 82)?)).ok_or(Error::Limit)?;
        let end = version_end.checked_add(usize::from(u16_at(data, 84)?)).ok_or(Error::Limit)?;
        if end != data.len()
           || name_end - cursor > crate::MAX_NAME_BYTES
           || version_end - name_end > 64
           || end - version_end > 128
        {
            return Err(Error::Limit);
        }
        let result =
            Self { kind,
                   kernel_abi: u32_at(data, 16)?,
                   broker_abi: u32_at(data, 20)?,
                   target,
                   identity,
                   code_type,
                   code_id,
                   code_digest: bytes(data, 88)?,
                   heap_bytes: u32_at(data, 72)?,
                   stack_bytes: u32_at(data, 76)?,
                   name: text(data.get(cursor..name_end).ok_or(Error::Malformed)?)?,
                   version: text(data.get(name_end..version_end).ok_or(Error::Malformed)?)?,
                   provenance: text(data.get(version_end..end).ok_or(Error::Malformed)?)?,
                   offers,
                   dependencies,
                   services,
                   devices };
        if result.name.is_empty()
           || result.version.is_empty()
           || result.kernel_abi == 0
           || result.broker_abi == 0
        {
            return Err(Error::Contract);
        }
        for (index, offer) in result.offers().enumerate()
        {
            let offer = offer?;
            offer.validate()?;
            if result.offers()
                     .take(index)
                     .any(|prior| prior.is_ok_and(|prior| prior.class == offer.class))
            {
                return Err(Error::Duplicate);
            }
        }
        for (index, service) in result.services().enumerate()
        {
            let service = service?;
            if result.services().take(index).any(|prior| {
                                                prior.is_ok_and(|prior| {
                                                         prior.service == service.service
                                                     })
                                            })
            {
                return Err(Error::Duplicate);
            }
        }
        for (index, dependency) in result.dependencies().enumerate()
        {
            let dependency = dependency?;
            if result.dependencies().take(index).any(|prior| {
                                                    prior.is_ok_and(|prior| {
                                                             prior.module == dependency.module
                                                             && prior.class == dependency.class
                                                         })
                                                })
            {
                return Err(Error::Duplicate);
            }
        }
        for device in result.devices()
        {
            device?;
        }
        Ok(result)
    }

    pub fn offers(self) -> impl Iterator<Item = Result<Offer>> + 'a
    {
        self.offers.as_chunks::<OFFER_BYTES>().0.iter().map(offer)
    }

    pub fn dependencies(self) -> impl Iterator<Item = Result<Dependency>> + 'a
    {
        self.dependencies.as_chunks::<DEPENDENCY_BYTES>().0.iter().map(dependency)
    }

    pub fn services(self) -> impl Iterator<Item = Result<Service>> + 'a
    {
        self.services.as_chunks::<SERVICE_BYTES>().0.iter().map(service)
    }

    pub fn devices(self) -> impl Iterator<Item = Result<DeviceMatch>> + 'a
    {
        self.devices.as_chunks::<MATCH_BYTES>().0.iter().map(device)
    }
}
fn offer(b: &[u8; OFFER_BYTES]) -> Result<Offer>
{
    Ok(Offer { class:          ClassId(bytes(b, 0)?),
               version:        u32_at(b, 4)?,
               request_schema: u32_at(b, 8)?,
               reply_schema:   u32_at(b, 12)?,
               max_request:    u32_at(b, 16)?,
               max_reply:      u32_at(b, 20)?,
               flags:          u64_at(b, 24)?,
               capabilities:   u64_at(b, 32)? })
}
fn dependency(b: &[u8; DEPENDENCY_BYTES]) -> Result<Dependency>
{
    let module = bytes(b, 0)?;
    let class = ClassId(bytes(b, 16)?);
    let version = u32_at(b, 20)?;
    let flags = u32_at(b, 24)?;
    if class.0 == [0; 4] || version == 0 || flags > 1 || b[28..32] != [0; 4]
    {
        return Err(Error::Contract);
    }
    Ok(Dependency { module,
                    class,
                    version,
                    optional: flags != 0 })
}
fn service(b: &[u8; SERVICE_BYTES]) -> Result<Service>
{
    let service = ClassId(bytes(b, 0)?);
    let version = u32_at(b, 4)?;
    let flags = u32_at(b, 8)?;
    if service.0 == [0; 4] || version == 0 || flags > 1 || b[12..16] != [0; 4]
    {
        return Err(Error::Contract);
    }
    Ok(Service { service,
                 version,
                 optional: flags != 0 })
}
fn device(b: &[u8; MATCH_BYTES]) -> Result<DeviceMatch>
{
    let bus = ClassId(bytes(b, 0)?);
    let value = bytes::<16>(b, 8)?;
    let mask = bytes::<16>(b, 24)?;
    if bus.0 == [0; 4]
       || b[4..8] != [0; 4]
       || value.iter().zip(mask).any(|(&value, mask)| value & !mask != 0)
    {
        return Err(Error::Contract);
    }
    Ok(DeviceMatch { bus,
                     value,
                     mask })
}
