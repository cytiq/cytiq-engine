#![no_main]

use libfuzzer_sys::fuzz_target;

#[allow(dead_code)]
#[path = "../checks.rs"]
mod checks;

fuzz_target!(|bytes: &[u8]| checks::gatingml(bytes));
