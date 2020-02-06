#[derive(structopt::StructOpt)]
struct Args {
    #[structopt(long)]
    dry_run: bool,
}

#[paw::main]
fn main(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    println!("Hello, world!");

    Ok(())
}
