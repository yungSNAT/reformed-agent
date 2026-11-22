#![no_main]
//! A webhook request, as it arrives from the network, and a route's prompt template.
//!
//! The first line of the input is the template; the rest is the raw request.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let split = data.iter().position(|&b| b == b'\n').unwrap_or(data.len());
    let template = String::from_utf8_lossy(&data[..split]);
    let request = data.get(split + 1..).unwrap_or_default();
    hermes_gateway::platforms::webhook::fuzz_request(request, &template);
});
