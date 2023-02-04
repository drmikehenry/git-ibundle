use std::io::Write;

fn vec_push_oid(v: &mut Vec<u8>, oid: git2::Oid) {
    v.extend_from_slice(&oid.to_string().as_str().as_bytes());
}

fn push_ref(v: &mut Vec<u8>, commit_id: git2::Oid, name: &[u8]) {
    vec_push_oid(v, commit_id);
    v.push(b' ');
    v.extend_from_slice(name);
    v.push(b'\n');
}

fn main() -> anyhow::Result<()> {
    let repo_path = ".";
    let repo = git2::Repository::open(repo_path)?;
    let head = repo.resolve_reference_from_short_name("HEAD")?;

    let mut pack_v = Vec::new();
    let mut v = Vec::new();
    v.extend_from_slice(b"# v2 git bundle\n");
    for r in repo.references()? {
        let r = r?;
        let commit = r.peel_to_commit()?;
        let commit_id = commit.id();
        push_ref(&mut v, commit_id, r.name_bytes());
        vec_push_oid(&mut pack_v, commit_id);
    }
    push_ref(&mut v, head.peel_to_commit()?.id(), b"HEAD");

    v.push(b'\n');

    let mut f = std::fs::File::create("output.bundle")?;
    f.write_all(&v)?;

    let mut child = std::process::Command::new("git")
        .args(["pack-objects", "--stdout", "--thin", "--delta-base-offset"])
        .stdin(std::process::Stdio::piped())
        .stdout(f)
        .spawn()?;

    let mut child_stdin = child.stdin.take().unwrap();
    child_stdin.write_all(&pack_v)?;
    drop(child_stdin);

    let exit_status = child.wait()?;
    println!("{:?}", exit_status);

    Ok(())
}
