
include!(concat!(env!("OUT_DIR"), "/bundled_std.rs"));

pub(crate) fn source(name: &str) -> Option<&'static str>
{
    MODULES.iter().find_map(|(module, source)| (*module == name).then_some(*source))
}

pub(super) fn has_children(name: &str) -> bool
{
    let prefix = format!("{}/", name);
    MODULES
        .iter()
        .any(|(module, _)| module.starts_with(&prefix))
}
