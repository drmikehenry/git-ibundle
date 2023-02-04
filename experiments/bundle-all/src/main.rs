fn push_ref(v: &mut Vec<u8>, commit_id: git2::Oid, name: &[u8]) {
    v.extend_from_slice(commit_id.to_string().as_str().as_bytes());
    v.push(b' ');
    v.extend_from_slice(name);
    v.push(b'\n');
}

fn main() -> anyhow::Result<()> {
    let repo_path = ".";
    let repo = git2::Repository::open(repo_path)?;
    let head = repo.resolve_reference_from_short_name("HEAD")?;
    let mut pack = repo.packbuilder()?;
    pack.set_threads(16);
    let mut walk = repo.revwalk()?;
    let mut v = Vec::new();
    v.extend_from_slice(b"# v2 git bundle\n");
    for r in repo.references()? {
        let r = r?;
        let commit = r.peel_to_commit()?;
        let commit_id = commit.id();
        walk.push(commit_id)?;
        push_ref(&mut v, commit_id, r.name_bytes());
    }
    push_ref(&mut v, head.peel_to_commit()?.id(), b"HEAD");

    v.push(b'\n');

    pack.set_progress_callback(|stage, n, m| {
        println!("{:?} {} {}", stage, n, m);
        true
    })?;
    pack.insert_walk(&mut walk)?;
    pack.foreach(|buf| {
        v.extend_from_slice(buf);
        true
    })?;
    std::fs::write("output.bundle", &v)?;
    Ok(())
}
