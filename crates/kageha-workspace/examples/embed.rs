//! Another framework needs only this component and the public contracts.
use kageha_contracts::execution::Control;
use kageha_workspace::ReadWorkspace;
use std::{path::Path, sync::atomic::AtomicBool};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().collect::<Vec<_>>();
    let workspace = ReadWorkspace::new(Path::new(args.get(1).ok_or("Supply root")?))?;
    let cancel = AtomicBool::new(false);
    let text = workspace.read_text(
        args.get(2).ok_or("Supply path")?,
        &Control {
            cancelled: &cancel,
            deadline: None,
        },
    )?;
    print!("{text}");
    Ok(())
}
