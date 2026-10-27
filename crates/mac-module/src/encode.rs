//! Shared writer used by the packager; no host or allocation dependency.
use crate::*;

pub struct Spec<'a>
{
    pub kind:         PackageKind,
    pub kernel_abi:   u32,
    pub target:       Target,
    pub identity:     [u8; 16],
    pub code_id:      i16,
    pub code_digest:  [u8; 32],
    pub heap_bytes:   u32,
    pub stack_bytes:  u32,
    pub name:         &'a str,
    pub version:      &'a str,
    pub provenance:   &'a str,
    pub offers:       &'a [Offer],
    pub dependencies: &'a [Dependency],
    pub services:     &'a [Service],
    pub devices:      &'a [DeviceMatch]
}
impl Spec<'_>
{
    pub fn encoded_len(&self) -> Result<usize>
    {
        let mut length = HEADER_BYTES;
        for (count, size) in [(self.offers.len(), OFFER_BYTES),
                              (self.dependencies.len(), DEPENDENCY_BYTES),
                              (self.services.len(), SERVICE_BYTES),
                              (self.devices.len(), MATCH_BYTES)]
        {
            if count > MAX_RECORDS
            {
                return Err(Error::Limit);
            }
            length = length.checked_add(count * size).ok_or(Error::Limit)?;
        }
        for (text, limit) in [(self.name, 128), (self.version, 64), (self.provenance, 128)]
        {
            if text.len() > limit
            {
                return Err(Error::Limit);
            }
            length = length.checked_add(text.len()).ok_or(Error::Limit)?;
        }
        if length > MAX_BYTES
        {
            return Err(Error::Limit);
        }
        Ok(length)
    }

    /// Only the returned prefix belongs to the encoded contract. Invalid specs
    /// return an error and must never be published by the caller.
    pub fn encode<'a>(&self, output: &'a mut [u8]) -> Result<&'a [u8]>
    {
        let length = self.encoded_len()?;
        let data = output.get_mut(..length).ok_or(Error::Limit)?;
        data.fill(0);
        put(data, 0, b"KMOD");
        put(data, 4, &SCHEMA.to_le_bytes());
        put(data, 6, &(HEADER_BYTES as u16).to_le_bytes());
        put(data, 8, &(length as u32).to_le_bytes());
        put(data, 12, &self.kind.file_type());
        put(data, 16, &self.kernel_abi.to_le_bytes());
        put(data, 20, &BROKER_ABI.to_le_bytes());
        put(data, 24, &self.target.architecture.to_le_bytes());
        data[26] = self.target.pointer_bits;
        data[27] = self.target.endian;
        put(data, 28, &self.target.convention.to_le_bytes());
        put(data, 32, &self.target.features.to_le_bytes());
        put(data, 40, &self.identity);
        put(data, 56, &self.kind.code_type());
        put(data, 60, &self.code_id.to_le_bytes());
        for (offset, count) in [(62, self.offers.len()),
                                (64, self.dependencies.len()),
                                (66, self.services.len()),
                                (68, self.devices.len())]
        {
            put(data, offset, &(count as u16).to_le_bytes());
        }
        put(data, 72, &self.heap_bytes.to_le_bytes());
        put(data, 76, &self.stack_bytes.to_le_bytes());
        for (offset, text) in [(80, self.name), (82, self.version), (84, self.provenance)]
        {
            put(data, offset, &(text.len() as u16).to_le_bytes());
        }
        put(data, 88, &self.code_digest);
        put(data, 120, &abi::LAYOUT_ID.to_le_bytes());
        let mut at = HEADER_BYTES;
        for offer in self.offers
        {
            put(data, at, &offer.class.0);
            for (offset, value) in [(4, offer.version),
                                    (8, offer.request_schema),
                                    (12, offer.reply_schema),
                                    (16, offer.max_request),
                                    (20, offer.max_reply)]
            {
                put(data, at + offset, &value.to_le_bytes());
            }
            put(data, at + 24, &offer.flags.to_le_bytes());
            put(data, at + 32, &offer.capabilities.to_le_bytes());
            at += OFFER_BYTES;
        }
        for dependency in self.dependencies
        {
            put(data, at, &dependency.module);
            put(data, at + 16, &dependency.class.0);
            put(data, at + 20, &dependency.version.to_le_bytes());
            put(data, at + 24, &u32::from(dependency.optional).to_le_bytes());
            at += DEPENDENCY_BYTES;
        }
        for service in self.services
        {
            put(data, at, &service.service.0);
            put(data, at + 4, &service.version.to_le_bytes());
            put(data, at + 8, &u32::from(service.optional).to_le_bytes());
            at += SERVICE_BYTES;
        }
        for device in self.devices
        {
            put(data, at, &device.bus.0);
            put(data, at + 8, &device.value);
            put(data, at + 24, &device.mask);
            at += MATCH_BYTES;
        }
        for text in [self.name, self.version, self.provenance]
        {
            put(data, at, text.as_bytes());
            at += text.len();
        }
        Contract::decode(data)?;
        Ok(data)
    }
}
fn put(data: &mut [u8], at: usize, bytes: &[u8])
{
    data[at..at + bytes.len()].copy_from_slice(bytes);
}
