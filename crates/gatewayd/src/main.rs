#![forbid(unsafe_code)]

fn main() {
    println!("gatewayd: {}", gateway_core::ping());
}
