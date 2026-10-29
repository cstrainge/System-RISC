//! Ordinary APPL workload. Body completion is register/service evidence only;
//! host PC/SP observations must separately establish actual CPU placement.
use core::{fmt::Write, sync::atomic::AtomicU64};

use mac_toolbox::{Cnid, Error, Rect, System, files::Fork, windows::WindowSpec};

#[path = "../../../riscv_smp_body.rs"]
mod body;
core::arch::global_asm!(include_str!("../../../riscv_smp_body.S"),
                        "SMP_BODY __smp_app_a_body, __smp_app_a_progress",
                        "SMP_BODY __smp_app_b_body, __smp_app_b_progress");

#[used]
#[unsafe(no_mangle)]
static __smp_app_a_progress: AtomicU64 = AtomicU64::new(0);
#[used]
#[unsafe(no_mangle)]
static __smp_app_b_progress: AtomicU64 = AtomicU64::new(0);
unsafe extern "C" {
    fn __smp_app_a_body(ticks: u64, seed: u64, ecalls: usize, output: *mut body::Capture);
    fn __smp_app_b_body(ticks: u64, seed: u64, ecalls: usize, output: *mut body::Capture);
}
type Result<T> = core::result::Result<T, Error>;
fn log(system: &System<'_>, args: core::fmt::Arguments<'_>) -> Result<()>
{
    let mut text = crate::LogBuffer::new();
    text.write_fmt(args).map_err(|_| Error::BadAbi)?;
    system.log(text.text())
}
fn require(system: &System<'_>, value: bool, cause: &str) -> Result<()>
{
    if value
    {
        return Ok(());
    }
    log(system, format_args!("SMP_APP_INVARIANT_FAIL cause={cause}\n"))?;
    Err(Error::BadAbi)
}

pub fn run(system: &System<'_>) -> Result<()>
{
    let me = system.processes().current()?;
    let a = system.files().lookup(me.volume, Cnid::ROOT, "Applications/SMP App A")?;
    let b = system.files().lookup(me.volume, Cnid::ROOT, "Applications/SMP App B")?;
    require(system, me.cnid == a.id || me.cnid == b.id, "unexpected actual source identity")?;
    let second = me.cnid == b.id;
    let probe: body::Probe = if second { __smp_app_b_body } else { __smp_app_a_body };
    // Like the existing native architecture/worker diagnostic, this workload
    // targets QEMU virt's 10 MHz counter. The external observer must separately
    // establish actual eligibility, execution and migration. No completion
    // marker here is an SMP acceptance result, even with four physical CPUs.
    let ticks = 20_000_000;
    if !second
    {
        let child = system.processes().launch(me.volume, b.id)?;
        require(system,
                child.id != me.id && child.thread != me.thread,
                "child is not a distinct actual thread")?;
    }
    let mut window =
        system.windows().create_owned(WindowSpec::new(Rect::new(90 + i16::from(second) * 35,
                                                                 110 + i16::from(second) * 40,
                                                                 230,
                                                                 410),
                                                       if second
                                                       {
                                                           "SMP App B"
                                                       }
                                                       else
                                                       {
                                                           "SMP App A"
                                                       }))?;
    log(system,
        format_args!("SMP_APP_BEGIN actor={} app={} thread={} cnid={} target_timebase=10000000 \
                      body={} body_entry=0x{:x}\n",
                     u8::from(second),
                     me.id.0,
                     me.thread.0,
                     me.cnid.0,
                     if second { "__smp_app_b_body" } else { "__smp_app_a_body" },
                     probe as usize))?;
    for phase in 0..6u64
    {
        let seed = me.id.0 ^ (phase << 40) ^ 0xa5a5_5a5a_1357_2468;
        // SAFETY: this is one adjacent macro instance with one live invocation
        // per real application. No Rust call can clobber the held sentinels.
        let vlenb = match unsafe { body::hold(probe, ticks, seed) }
        {
            Ok(value) => value,
            Err(failure) =>
            {
                log(system,
                    format_args!("SMP_APP_REGISTER_FAIL app={} phase={phase} cause={failure}\n",
                                 me.id.0))?;
                return Err(Error::BadAbi);
            }
        };
        let expected = [u8::from(second) + phase as u8; 64];
        let mut handle = system.memory().handle_from_bytes(&expected)?;
        handle.resize(8192)?;
        let mut actual = [0; 64];
        require(system,
                handle.read(0, &mut actual)? == actual.len() && actual == expected,
                "handle bytes changed across resident resize")?;
        handle.dispose()?;
        let mut fork = system.files().open(me.volume, me.cnid, Fork::Resource)?;
        require(system, fork.read(&mut actual)? > 0, "actual application resource fork is empty")?;
        fork.close()?;
        let mut text = [0; 64];
        require(system,
                system.resources().application().string(2000, &mut text)? == "process-spinner",
                "resource chain selected another application")?;
        require(system,
                matches!(system.files().lookup(me.volume, Cnid::ROOT, "Missing SMP file"),
                         Err(Error::NotFound)),
                "missing-file refusal lost its typed cause")?;
        let after = system.processes().current()?;
        require(system,
                after.id == me.id && after.thread == me.thread,
                "service changed actual caller identity")?;
        let mut title = crate::LogBuffer::new();
        write!(title, "SMP {} phase {phase}", if second { "B" } else { "A" }).map_err(|_| {
                                                                                 Error::BadAbi
                                                                             })?;
        window.set_title(title.text())?;
        log(system,
            format_args!("SMP_APP_PHASE_DONE app={} thread={} phase={phase} fv=preserved \
                          vlenb={vlenb} services=memory-file-resource-window\n",
                         me.id.0, me.thread.0))?;
    }
    window.close()?;
    log(system,
        format_args!("SMP_APP_BODY_DONE app={} thread={} phases=6 \
                      placement=requires-host-evidence\n",
                     me.id.0, me.thread.0))
}
