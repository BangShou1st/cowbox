#![forbid(unsafe_code)]

fn main() {
    println!("cowbox {}", env!("CARGO_PKG_VERSION"));
}
