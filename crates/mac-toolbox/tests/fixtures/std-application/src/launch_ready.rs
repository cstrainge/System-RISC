//! Real application caller of the normal launch trap while the target's actual
//! virtual RNG source is delayed by the private guest's hardware backend.
use std::{error::Error, sync::mpsc, thread, time::Duration};

use mac_toolbox::System;

pub fn run(system: &System) -> Result<(), Box<dyn Error>>
{
    let caller = system.processes().current()?;
    system.log(&format!("READY_CALLER_BEGIN app={} thread={}\n", caller.id.0, caller.thread.0))?;
    let parent = system.files().catalog(caller.volume, caller.cnid)?.parent;
    let target = system.files().lookup(caller.volume, parent, "Ready Target")?;
    let (send, receive) = mpsc::channel();
    let volume = caller.volume;
    let cnid = target.id;
    let child = thread::spawn(move || {
        let system = System::new().expect("launch caller child System admission invariant");
        let current = system.client().current_native_thread().unwrap();
        system.log(&format!("READY_CALLER_CHILD thread={}\n", current.0)).unwrap();
        send.send(()).unwrap();
        let launched = system.processes().launch(volume, cnid).unwrap();
        system.log(&format!("READY_CALLER_ACCEPTED target={} thread={}\n",
                            launched.id.0, launched.thread.0))
              .unwrap();
        (launched.id.0, launched.thread.0)
    });
    receive.recv()?;
    thread::sleep(Duration::from_secs(1));
    assert!(!system.processes().is_running(volume, cnid)?,
            "target author became live while its required source was pending");
    system.log(&format!("READY_CALLER_PENDING target_cnid={}\n", cnid.0))?;
    #[cfg(feature = "launch-cancel")]
    {
        drop(child);
        system.log("READY_CALLER_EXIT_PENDING code=-41\n")?;
        std::process::exit(-41);
    }
    #[cfg(not(feature = "launch-cancel"))]
    {
        let (app, thread) = child.join().expect("normal launch child returns after READY");
        assert_ne!(app, caller.id.0);
        assert_ne!(thread, 0);
        system.log("READY_CALLER_JOINED\n")?;
        Ok(())
    }
}
