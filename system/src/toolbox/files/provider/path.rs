//! Bounded ancestry construction without repeated prefix copying or one heap
//! allocation per component. Root names are not part of native slash paths.
#![forbid(unsafe_code)]
use super::*;
#[derive(Default)]
pub(super) struct Builder
{
    parts: Vec<(u32, usize)>,
    names: Vec<u8>
}
impl Builder
{
    pub(super) fn push(&mut self, entry: &CatalogInfo) -> Result<(), Status>
    {
        if self.parts.len() == 255 || self.parts.iter().any(|(id, _)| *id == entry.id)
        {
            return Err(NOT_SUPPORTED);
        }
        let name = entry.name();
        if name.contains('/')
        {
            return Err(NOT_SUPPORTED);
        }
        let length = self.names
                         .len()
                         .checked_add(name.len())
                         .and_then(|n| n.checked_add(self.parts.len() + 1))
                         .ok_or(BAD_ARGUMENT)?;
        if length > super::super::MAX_PATH
        {
            return Err(BAD_ARGUMENT);
        }
        self.names.try_reserve(name.len()).map_err(|_| NO_MEMORY)?;
        self.parts.try_reserve(1).map_err(|_| NO_MEMORY)?;
        self.names.extend_from_slice(name.as_bytes());
        self.parts.push((entry.id, self.names.len()));
        Ok(())
    }

    pub(super) fn finish(self) -> Result<Vec<u8>, Status>
    {
        let length = (self.names.len() + self.parts.len()).max(1);
        let mut path = Vec::new();
        path.try_reserve_exact(length).map_err(|_| NO_MEMORY)?;
        if self.parts.is_empty()
        {
            path.push(b'/');
        }
        for (at, &(_, end)) in self.parts.iter().enumerate().rev()
        {
            let start = if at == 0 { 0 } else { self.parts[at - 1].1 };
            path.push(b'/');
            path.extend_from_slice(&self.names[start..end]);
        }
        Ok(path)
    }
}
pub(super) fn component(path: &str, mut at: usize) -> Option<(usize, &str)>
{
    while path.as_bytes().get(at) == Some(&b'/')
    {
        at += 1;
    }
    if at == path.len()
    {
        return None;
    }
    let end = path[at..].find('/').map_or(path.len(), |end| at + end);
    Some((end, &path[at..end]))
}
#[cfg(test)]
mod tests
{
    use super::*;
    fn entry(id: u32, name: &str) -> CatalogInfo
    {
        let mut info = CatalogInfo { id,
                                     ..Default::default() };
        info.name_length = utf8_name(name.as_bytes(), &mut info.name);
        info
    }
    #[test]
    fn ancestry_emits_once_in_reverse_and_rejects_cycles_depth_and_length()
    {
        assert_eq!(Builder::default().finish().unwrap(), b"/");
        let mut path = Builder::default();
        path.push(&entry(17, "Café")).unwrap();
        path.push(&entry(16, "Tools")).unwrap();
        assert_eq!(path.push(&entry(17, "Café")), Err(NOT_SUPPORTED));
        assert_eq!(path.finish().unwrap(), "/Tools/Café".as_bytes());
        let mut path = Builder::default();
        assert_eq!(path.push(&entry(16, "a/b")), Err(NOT_SUPPORTED));
        for id in 16..271
        {
            path.push(&entry(id, "a")).unwrap();
        }
        assert_eq!(path.push(&entry(271, "a")), Err(NOT_SUPPORTED));
        let mut path = Builder::default();
        let name = "a".repeat(255);
        for id in 16..32
        {
            path.push(&entry(id, &name)).unwrap();
        }
        assert_eq!(path.push(&entry(32, "b")), Err(BAD_ARGUMENT));
        assert_eq!(path.finish().unwrap().len(), 4096);
        let mut at = 0;
        let mut actual = Vec::new();
        while let Some((next, text)) = component("//Café///../Tools/", at)
        {
            actual.push(text);
            at = next;
        }
        assert_eq!(actual, ["Café", "..", "Tools"]);
    }
}
