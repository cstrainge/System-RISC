//! Actual std collection seeding. Never record source bytes or hash seeds.
#[cfg(not(feature = "std-random-unavailable"))]
use std::thread;
use std::{collections::{HashMap, HashSet},
          error::Error};

use mac_toolbox::System;

fn collections()
{
    for round in 0..32u64
    {
        let mut map = HashMap::new();
        let mut set = HashSet::new();
        for key in 0..128u64
        {
            assert!(map.insert(key, key ^ round).is_none());
            assert!(set.insert(key));
        }
        for key in 0..128u64
        {
            assert_eq!(map.remove(&key), Some(key ^ round));
            assert!(set.remove(&key));
        }
        assert!(map.is_empty() && set.is_empty());
    }
}

pub(super) fn run(system: &System) -> Result<(), Box<dyn Error>>
{
    system.log("STD_PROBE_RANDOM_BEGIN\n")?;
    #[cfg(feature = "std-random-unavailable")]
    {
        // HashMap's infallible seed requirement must report/Abort when the
        // source is unavailable. Returning here is a native scenario failure.
        collections();
        panic!("std random seeding succeeded without an entropy provider");
    }
    #[cfg(not(feature = "std-random-unavailable"))]
    {
        // This diagnostic checks the real source explicitly. Launch admission
        // readiness remains a separate production gate before app migration.
        system.entropy().ready()?;
        collections();
        let children: Vec<_> = (0..4).map(|_| {
                                         thread::spawn(|| {
                                             let system = System::new().unwrap();
                                             let identity =
                                                 system.client().current_native_thread().unwrap().0;
                                             collections();
                                             identity
                                         })
                                     })
                                     .collect();
        let mut ids: Vec<_> = children.into_iter().map(|child| child.join().unwrap()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 4);
        system.log(&format!("STD_PROBE_RANDOM_COLLECTIONS children={ids:?} rounds=32 \
                             entries=128\n"))?;
        Ok(())
    }
}
