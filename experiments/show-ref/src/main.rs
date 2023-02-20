use std::io::Write;

use anyhow::bail;
use bstr::BString;

type AResult<T> = anyhow::Result<T>;

fn main() -> AResult<()> {
    let repo = git2::Repository::open(".")?;
    let mut handle = std::io::stdout().lock();
    for r in repo.references()? {
        let r = r?;
        let oid = if let Some(oid) = r.target() {
            oid
        } else {
            bail!("found non-direct ref kind {:?}", r.kind());
        };
        let name = BString::from(r.name_bytes());
        // println!("{:?} {:?}", oid, name);
        let s = format!("{:?} {:?}\n", oid, name);
        handle.write_all(s.as_bytes())?;
    }
    Ok(())
}
