use anyhow::Context;
use vergen::EmitBuilder;

fn main() -> anyhow::Result<()> {
    EmitBuilder::builder()
        .all_build()
        .all_cargo()
        .all_git()
        .emit()
        .context("Unable to generate the cargo keys!")
}
