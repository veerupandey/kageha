use std::{
    io::{self, Read},
    path::Path,
};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().collect::<Vec<_>>();
    match args.get(1).map(String::as_str){
  Some("extract-document-stdin")=>{
   kageha_workspace::worker_limits();
   let suffix=args.get(2).ok_or("Supply document extension")?;let mut bytes=Vec::new();io::stdin().take((kageha_workspace::DOCUMENT_LIMIT+1) as u64).read_to_end(&mut bytes)?;
   print!("{}",kageha_workspace::documents::extract_bytes(&bytes,suffix)?);
  },
  Some("extract-document")=>{kageha_workspace::worker_limits();print!("{}",kageha_workspace::documents::extract(Path::new(args.get(2).ok_or("Supply document path")?))?);},
  Some("read")=>{let root=Path::new(args.get(2).ok_or("Supply workspace root")?);let path=args.get(3).ok_or("Supply file path")?;let workspace=kageha_workspace::ReadWorkspace::new(root)?;print!("{}",workspace.read_text(path,&kageha_contracts::execution::Control{cancelled:&std::sync::atomic::AtomicBool::new(false),deadline:None})?);},
  _=>return Err("Usage: kageha-workspace read ROOT PATH | extract-document PATH | extract-document-stdin EXT".into())
 }
    Ok(())
}
