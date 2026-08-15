#![no_main]

use libfuzzer_sys::fuzz_target;
use proofframe::receipt::{TrustPolicy, verify_json_with_policy};

const MAX_INPUT_BYTES: usize = 1024 * 1024;

fuzz_target!(|data: &[u8]| {
    if data.len() <= MAX_INPUT_BYTES {
        if let Ok(source) = std::str::from_utf8(data) {
            let _ = verify_json_with_policy(source, &TrustPolicy::SignatureOnly);
        }
    }
});
