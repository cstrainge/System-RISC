//! Optional metadata is an external-format distinction. Plain files keep their
//! host bytes and names; no import, rewrite or sidecar is needed to read them.
use super::*;
impl<R: Rpc> Filesystem<R>
{
    pub(super) async fn plain_node(&mut self, volume: u32, parent: u32, name: &[u8])
                                   -> Result<Node>
    {
        let reply = self.call(Op::Describe, parent, 0, 0, name).await?;
        let dates = wire::Description::decode(&reply.body).map_err(|_| Cause::Invalid)?;
        if reply.header.object == 0
        {
            return Err(Cause::Invalid.into());
        }
        let kind = match reply.header.value
        {
            1 => Kind::File,
            2 => Kind::Directory,
            _ => return Err(Cause::Invalid.into())
        };
        let mut metadata = Metadata::new(kind,
                                         reply.header.object,
                                         self.info(volume)?.metadata.volume,
                                         core::str::from_utf8(name).map_err(|_| Cause::Invalid)?,
                                         dates.created)?;
        metadata.modified = dates.modified;
        let layout =
            Layout::new(&metadata, if kind == Kind::File { reply.header.offset } else { 0 }, 0)?;
        let key = mac_hfs_b::names::comparison(&metadata.name).map_err(|_| Cause::Invalid)?;
        let mut host_name = buffer(name.len())?;
        host_name.copy_from_slice(name);
        Ok(Node { parent: 0,
                  metadata,
                  layout,
                  key,
                  host_name: Some(host_name) })
    }

    pub(super) async fn open_plain(&mut self, volume: u32, parent: u32, name: &[u8])
                                   -> Result<(u32, Node)>
    {
        let opened = self.call(Op::OpenRead, parent, 0, 0, name).await?;
        let handle = opened.header.object;
        let result =
            self.plain_node(volume, parent, name).await.and_then(|node| {
                                                           if opened.header.offset
                                                              != node.layout.data.length
                                                           {
                                                               return Err(Cause::Stale.into());
                                                           }
                                                           Ok(node)
                                                       });
        match result
        {
            Ok(node) => Ok((handle, node)),
            Err(error) => self.cleanup(handle, Err(error)).await
        }
    }

    pub(super) async fn candidate(&mut self, volume: u32, parent: u32, name: &[u8])
                                  -> Result<Node>
    {
        let opened = self.call(Op::OpenRead, parent, 0, 0, name).await?;
        let handle = opened.header.object;
        let result =
            async {
                let reply =
                    self.call(Op::Read, handle, 0, forks::METADATA_BYTES as u32, &[]).await?;
                if !reply.body.starts_with(forks::MAGIC)
                {
                    return self.plain_node(volume, parent, name).await;
                }
                let bytes = reply.body
                                 .as_slice()
                                 .try_into()
                                 .map_err(|_| Cause::Format(forks::Error::Corrupt))?;
                let (metadata, layout) = forks::decode_metadata(bytes, opened.header.offset)?;
                let key = mac_hfs_b::names::comparison(&metadata.name).map_err(|_| Cause::Invalid)?;
                Ok(Node { parent: 0,
                          metadata,
                          layout,
                          key,
                          host_name: None })
            }.await;
        self.cleanup(handle, result).await
    }
}
