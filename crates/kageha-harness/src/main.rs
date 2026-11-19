fn main() -> Result<(), Box<dyn std::error::Error>> {
    kageha_harness::run(std::env::args().skip(1))
}
