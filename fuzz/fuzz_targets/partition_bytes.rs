#![no_main]

use libfuzzer_sys::fuzz_target;
use proofframe::fuzz_partition_bytes;

fuzz_target!(|data: &[u8]| {
    let _ = fuzz_partition_bytes(data);
});
