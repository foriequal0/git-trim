use vergen_gix::{Build, Cargo, Emitter, Gix, Rustc, Sysinfo};

fn main() -> anyhow::Result<()> {
    Emitter::default()
        .add_instructions(&Build::all_build())?
        .add_instructions(&Cargo::all_cargo())?
        .add_instructions(&Gix::all_git())?
        .add_instructions(&Rustc::all_rustc())?
        .add_instructions(&Sysinfo::all_sysinfo())?
        .emit()
}
