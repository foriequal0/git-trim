use vergen::{vergen, Config};

fn main() {
    // Generate the 'cargo:' key output
    vergen(Config::default()).expect("Unable to generate the cargo keys!");
}
