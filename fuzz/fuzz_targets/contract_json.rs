#![no_main]

use libfuzzer_sys::fuzz_target;
use proofframe::ContractAst;

const MAX_INPUT_BYTES: usize = 1024 * 1024;

fuzz_target!(|data: &[u8]| {
    if data.len() <= MAX_INPUT_BYTES {
        if let Ok(source) = std::str::from_utf8(data) {
            let _ = ContractAst::from_json(source);
        }
    }
});
