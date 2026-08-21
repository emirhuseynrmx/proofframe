#![no_main]

use libfuzzer_sys::fuzz_target;
use proofframe::fuzz_partition_manifest_json;

const MAX_INPUT_BYTES: usize = 1024 * 1024;

fuzz_target!(|data: &[u8]| {
    if data.len() <= MAX_INPUT_BYTES {
        let _ = fuzz_partition_manifest_json(data);
    }
});
