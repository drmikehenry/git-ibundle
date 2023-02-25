use std::io::Write;

fn vec_push_oid(v: &mut Vec<u8>, oid: git2::Oid) {
    v.extend_from_slice(&oid.to_string().as_str().as_bytes());
}

fn push_ref(
    header: &mut Vec<u8>,
    pack_input: &mut Vec<u8>,
    commit_id: git2::Oid,
    name: &[u8],
) {
    vec_push_oid(header, commit_id);
    header.push(b' ');
    header.extend_from_slice(name);
    header.push(b'\n');
    vec_push_oid(pack_input, commit_id);
    pack_input.push(b'\n');
}

fn main() -> anyhow::Result<()> {
    let repo_path = ".";
    let repo = git2::Repository::open(repo_path)?;
    let head = repo.resolve_reference_from_short_name("HEAD")?;

    let mut pack_input = Vec::new();
    let mut header = Vec::new();
    header.extend_from_slice(b"# v2 git bundle\n");
    for r in repo.references()? {
        let r = r?;
        let commit = r.peel_to_commit()?;
        let commit_id = commit.id();
        push_ref(&mut header, &mut pack_input, commit_id, r.name_bytes());
    }
    push_ref(
        &mut header,
        &mut pack_input,
        head.peel_to_commit()?.id(),
        b"HEAD",
    );

    header.push(b'\n');

    let mut f = std::fs::File::create("output.bundle")?;
    f.write_all(&header)?;

    let mut child = std::process::Command::new("git")
        .args([
            "pack-objects",
            "--stdout",
            "--thin",
            "--delta-base-offset",
            "--threads=1",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(f)
        .spawn()?;

    let mut child_stdin = child.stdin.take().unwrap();
    child_stdin.write_all(&pack_input)?;
    drop(child_stdin);

    let exit_status = child.wait()?;
    println!("{:?}", exit_status);

    Ok(())
}
