//! Architecture-only assertions alongside canonical SDK process tests. Register
//! sentinels must survive a single assembly interval (not a Rust function call
//! whose ordinary ABI is allowed to clobber them). No test-only trap is added.
#![cfg(target_arch = "riscv64")]
use core::arch::global_asm;
global_asm!(include_str!("riscv_context_cases.S"));
#[repr(C)]
#[derive(Default)]
struct Capture
{
    code:     u64,
    observed: u64,
    vlenb:    u64
}
unsafe extern "C" {
    fn __toolbox_cpu_context_probe(ticks: u64, seed: u64, ecalls: usize, output: *mut Capture);
}
#[derive(Debug)]
pub enum Failure
{
    Floating
    {
        register: u8, expected: u64, observed: u64
    },
    Vector
    {
        register: u8, mismatching_bytes: u64
    },
    Csr
    {
        name: &'static str, expected: u64, observed: u64
    },
    ServiceRefusal
    {
        expected: i64, observed: i64
    },
    Instruction
    {
        extension: &'static str, observed: u64
    },
    Unknown
    {
        code: u64, observed: u64
    }
}
pub fn probe(ticks: u64, seed: u64, ecalls: bool) -> Result<u64, Failure>
{
    let mut capture = Capture::default();
    // SAFETY: the architecture probe uses only admitted RVA23 instructions,
    // retains no pointers, owns the output, and preserves the LP64 callee state.
    unsafe {
        __toolbox_cpu_context_probe(ticks, seed, usize::from(ecalls), &mut capture);
    }
    let Capture { code,
                  observed,
                  vlenb } = capture;
    match code
    {
        0 => Ok(vlenb),
        1..=32 =>
        {
            Err(Failure::Floating { register: (code - 1) as u8,
                                    expected: seed,
                                    observed })
        }
        33..=64 =>
        {
            Err(Failure::Vector { register:          (code - 33) as u8,
                                  mismatching_bytes: observed })
        }
        65..=69 =>
        {
            Err(Failure::Csr { expected: [seed & 31, 3, 0, 1, seed & 7][(code - 65) as usize],
                               name: ["fcsr", "vl", "vtype", "vstart", "vcsr"]
                                   [(code - 65) as usize],
                               observed })
        }
        70 =>
        {
            Err(Failure::ServiceRefusal { expected: mac_toolbox::abi::NOT_SUPPORTED as i64,
                                          observed: observed as i64 })
        }
        71..=73 =>
        {
            Err(Failure::Instruction { extension: ["Zba", "Zbb", "Zbs"][(code - 71) as usize],
                                       observed })
        }
        _ =>
        {
            Err(Failure::Unknown { code,
                                   observed })
        }
    }
}
