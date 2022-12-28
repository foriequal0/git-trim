use vergen::{vergen, Config};

fn main() {
    // Generate the 'cargo:' key output
    let mut config = Config::default();
    *config.git_mut().skip_if_error_mut() = true;
    vergen(config).expect("Unable to generate the cargo keys!");
}
