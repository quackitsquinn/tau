mod cfg;
mod instance;
mod invocation;

pub use cfg::GdbConfig;

pub use invocation::GdbInvocation;

use crate::gdb::instance::Gdb;

const BINARY_PATH: &str = "target/artifacts/tau_kernel.iso/boot/kernel.bin";

pub fn run_gdb(config: &mut GdbConfig) -> Gdb {
    let invocation = config.invocation.take().expect("GDB invocation not set");
    instance::start_gdb(config, &invocation)
}
