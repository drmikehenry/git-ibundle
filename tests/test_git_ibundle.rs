use std::collections;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::bail;
use assert_cmd::assert::Assert;
use assert_cmd::prelude::*;
use bstr::{BStr, BString, ByteSlice, ByteVec, B};
use tempfile;

type SeqNum = u64;

type AResult<T> = anyhow::Result<T>;

fn setup_test_dir() -> AResult<tempfile::TempDir> {
    let temp_dir_name = "tmp";
    let temp_dir_path = PathBuf::from(temp_dir_name);
    if !temp_dir_path.is_dir() {
        println!("{:?} must be a directory for testing.\n", temp_dir_path);
        println!("- To use a normal directory:");
        println!("");
        println!("    mkdir {}", temp_dir_name);
        println!("");
        if cfg!(unix) {
            println!(
                "- (unix) May use a symlink to use a different filesystem;"
            );
            println!(
                "  this may be useful for non-utf8 filename support.  E.g.:"
            );
            println!("");
            println!("    mkdir /run/user/$(id -u)/test-git-ibundle-tmp");
            println!(
                "    ln -s /run/user/$(id -u)/test-git-ibundle-tmp {}",
                temp_dir_name
            );
        }
        assert!(temp_dir_path.is_dir());
    }
    let dir = tempfile::Builder::new()
        .prefix("test")
        .rand_bytes(5)
        .tempdir_in(&temp_dir_path)?;
    Ok(dir)
}

#[derive(PartialEq, Eq, Debug)]
enum Head {
    Symbolic(BString),
    Detached(git2::Oid),
}

#[derive(PartialEq, Eq, Debug)]
struct RepoState {
    refs: collections::HashMap<BString, git2::Oid>,
    head: Head,
}

fn repo_state(repo_path: &Path) -> AResult<RepoState> {
    let repo = git2::Repository::open(repo_path)?;
    let mut refs = collections::HashMap::new();
    for r in repo.references()? {
        let r = r?;
        let oid = if let Some(oid) = r.target() {
            oid
        } else {
            bail!("found non-direct ref kind {:?}", r.kind());
        };
        refs.insert(BString::from(r.name_bytes()), oid);
    }
    let head_ref = repo.find_reference("HEAD")?;
    let head = if repo.head_detached()? {
        let head_commit_id = head_ref.target().unwrap();
        Head::Detached(head_commit_id)
    } else {
        Head::Symbolic(head_ref.symbolic_target_bytes().unwrap().into())
    };
    Ok(RepoState { refs, head })
}

fn must_git<I, S>(repo_path: &Path, args: I) -> Assert
where
    I: IntoIterator<Item = S>,
    S: AsRef<BStr>,
{
    let os_args = args
        .into_iter()
        .map(|a| a.as_ref().to_vec().into_os_string().unwrap())
        .collect::<Vec<_>>();

    Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(os_args)
        .env("GIT_AUTHOR_NAME", "author")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "committer")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "Fri, 11 Sep 2020 12:34:56 -0400")
        .env("GIT_COMMITTER_DATE", "Fri, 11 Sep 2020 12:34:56 -0400")
        .assert()
        .success()
}

fn must_git_fsck(repo_path: &Path) -> Assert {
    must_git(repo_path, ["fsck"])
}

fn must_git_checkout(repo_path: &Path, ref_name: impl AsRef<BStr>) -> Assert {
    must_git(repo_path, [B("checkout"), ref_name.as_ref()])
}

fn must_git_branch(
    repo_path: &Path,
    branch_name: impl AsRef<BStr>,
    ref_name: impl AsRef<BStr>,
) -> Assert {
    must_git(
        repo_path,
        [B("branch"), branch_name.as_ref(), ref_name.as_ref()],
    )
}

fn must_git_branch_delete(
    repo_path: &Path,
    branch_name: impl AsRef<BStr>,
) -> Assert {
    must_git(repo_path, [B("branch"), B("-D"), branch_name.as_ref()])
}

fn must_git_tag(
    repo_path: &Path,
    tag_name: impl AsRef<BStr>,
    ref_name: impl AsRef<BStr>,
) -> Assert {
    must_git(repo_path, [B("tag"), tag_name.as_ref(), ref_name.as_ref()])
}

fn must_git_atag(
    repo_path: &Path,
    tag_name: impl AsRef<BStr>,
    ref_name: impl AsRef<BStr>,
) -> Assert {
    let mut msg = BString::from("Annotated Tag ");
    msg.extend(tag_name.as_ref().iter());
    msg.push_str("\n\nMore\ncomments.");
    must_git(
        repo_path,
        [
            B("tag"),
            B("-m"),
            &msg,
            tag_name.as_ref(),
            ref_name.as_ref(),
        ],
    )
}

fn must_git_tag_delete(repo_path: &Path, tag_name: impl AsRef<BStr>) -> Assert {
    must_git(repo_path, [B("tag"), B("-d"), tag_name.as_ref()])
}

fn must_git_fsck_and_diff(
    dst_repo_path: &Path,
    src_repo_path: &Path,
) -> AResult<()> {
    must_git_fsck(dst_repo_path);
    let src_state = repo_state(src_repo_path)?;
    let dst_state = repo_state(dst_repo_path)?;
    assert_eq!(src_state, dst_state);
    Ok(())
}

fn must_git_commit_file(repo_path: &Path, commit_num: &mut usize) -> Assert {
    // TODO: env vars for repeatability?
    *commit_num += 1;
    let file_name = "file.txt";
    println!("{:?}", file_name);
    let mut f = fs::File::options()
        .create(true)
        .write(true)
        .append(true)
        .open(&repo_path.join(file_name))
        .unwrap();
    write!(f, "data-{}\n", commit_num).unwrap();
    drop(f);
    let mut msg = BString::from("Commit ");
    msg.extend(commit_num.to_string().into_bytes().into_iter());
    msg.push_str("\nSummary.\n\nMore\ncomments.\n");
    must_git(repo_path, [B("add"), file_name.as_bytes()]);
    must_git(repo_path, [B("commit"), B("-m"), &msg])
}

fn git_ibundle<I, S>(repo_path: &Path, args: I) -> Assert
where
    I: IntoIterator<Item = S>,
    S: AsRef<BStr>,
{
    let os_args = args
        .into_iter()
        .map(|a| a.as_ref().to_vec().into_os_string().unwrap())
        .collect::<Vec<_>>();

    Command::cargo_bin("git-ibundle")
        .unwrap()
        .current_dir(repo_path)
        .args(os_args)
        .assert()
}

fn must_ibundle<I, S>(repo_path: &Path, args: I) -> Assert
where
    I: IntoIterator<Item = S>,
    S: AsRef<BStr>,
{
    git_ibundle(repo_path, args).success()
}

fn fail_ibundle<I, S>(expected_status: i32, repo_path: &Path, args: I) -> Assert
where
    I: IntoIterator<Item = S>,
    S: AsRef<BStr>,
{
    let result = git_ibundle(repo_path, args);
    assert_eq!(result.get_output().status.code().unwrap(), expected_status);
    result
}

#[derive(PartialEq, Eq, Debug)]
struct IBundleStatus {
    repo_id: BString,
    max_seq_num: SeqNum,
    next_seq_num: SeqNum,
}

fn must_ibundle_status(repo_path: &Path) -> IBundleStatus {
    let stdout = git_ibundle(repo_path, ["status"])
        .get_output()
        .stdout
        .clone();

    let mut blines = stdout
        .as_bstr()
        .split_str(&b"\n")
        .filter_map(|s| s.as_bstr().split_once_str(b": "))
        .map(|(key, value)| (key.as_bstr(), value.as_bstr()));

    let (key, value) = blines.next().unwrap();
    assert_eq!(key, "repo_id");
    let repo_id = BString::from(value);

    let (key, value) = blines.next().unwrap();
    assert_eq!(key, "max_seq_num");
    let max_seq_num = value.to_str().unwrap().parse().unwrap();

    let (key, value) = blines.next().unwrap();
    assert_eq!(key, "next_seq_num");
    let next_seq_num = value.to_str().unwrap().parse().unwrap();

    assert_eq!(next_seq_num, max_seq_num + 1);

    IBundleStatus {
        repo_id,
        max_seq_num,
        next_seq_num,
    }
}

fn setup_src_dst_repos(
    test_dir: &tempfile::TempDir,
) -> AResult<(PathBuf, PathBuf)> {
    let src_dir = test_dir.path().join("src");
    let dst_dir = test_dir.path().join("dst.git");
    fs::DirBuilder::new().create(&src_dir)?;
    fs::DirBuilder::new().create(&dst_dir)?;

    must_git(&src_dir, ["init", "--initial-branch", "main"]);
    must_git(&dst_dir, ["init", "--initial-branch", "main", "--bare"]);
    Ok((src_dir, dst_dir))
}

fn setup() -> AResult<(tempfile::TempDir, PathBuf, PathBuf)> {
    let test_dir = setup_test_dir()?;
    let (src_dir, dst_dir) = setup_src_dst_repos(&test_dir)?;
    Ok((test_dir, src_dir, dst_dir))
}

#[test]
fn verify_initial_status() -> AResult<()> {
    let (_test_dir, src_dir, dst_dir) = setup()?;

    let initial_status = IBundleStatus {
        repo_id: BString::from("NONE"),
        max_seq_num: 0,
        next_seq_num: 1,
    };
    assert_eq!(must_ibundle_status(&src_dir), initial_status);
    assert_eq!(must_ibundle_status(&dst_dir), initial_status);
    Ok(())
}

#[test]
fn ibundle_empty_repo() -> AResult<()> {
    let (_test_dir, src_dir, dst_dir) = setup()?;
    must_ibundle(&src_dir, ["create", "../repo.ibundle"]);
    must_ibundle(&dst_dir, ["fetch", "../repo.ibundle"]);
    must_git_fsck_and_diff(&dst_dir, &src_dir)?;
    // Repeated fetch should succeed.
    must_ibundle(&dst_dir, ["fetch", "../repo.ibundle"]);
    must_git_fsck_and_diff(&dst_dir, &src_dir)?;
    let src_status = must_ibundle_status(&src_dir);
    let dst_status = must_ibundle_status(&dst_dir);
    assert_eq!(src_status.repo_id, dst_status.repo_id);
    Ok(())
}

#[test]
fn create_without_changes_is_empty() -> AResult<()> {
    let (_test_dir, src_dir, dst_dir) = setup()?;
    must_ibundle(&src_dir, ["create", "../repo.ibundle"]);
    must_ibundle(&dst_dir, ["fetch", "../repo.ibundle"]);
    assert_eq!(must_ibundle_status(&src_dir).max_seq_num, 1);
    assert_eq!(must_ibundle_status(&dst_dir).max_seq_num, 1);
    // Second create should fail unless `--allow-empty`.
    fail_ibundle(3, &src_dir, ["create", "../repo.ibundle"]);
    must_ibundle(&src_dir, ["create", "../repo.ibundle", "--allow-empty"]);
    must_ibundle(&dst_dir, ["fetch", "../repo.ibundle"]);
    assert_eq!(must_ibundle_status(&src_dir).max_seq_num, 2);
    assert_eq!(must_ibundle_status(&dst_dir).max_seq_num, 2);
    must_git_fsck(&dst_dir);
    Ok(())
}

fn make_repo_changes1(repo_path: impl AsRef<Path>, commit_num: &mut usize) {
    let repo_path = repo_path.as_ref();
    must_git_commit_file(repo_path, commit_num);
    must_git_branch(repo_path, "branch1", "HEAD");
    must_git_commit_file(repo_path, commit_num);
    must_git_tag(repo_path, "tag1", "HEAD");
    must_git_commit_file(repo_path, commit_num);
    must_git_atag(repo_path, "atag1", "HEAD");
}

#[test]
fn initial_changes() -> AResult<()> {
    let (_test_dir, src_dir, dst_dir) = setup()?;
    let mut commit_num = 0;
    make_repo_changes1(&src_dir, &mut commit_num);
    must_ibundle(&src_dir, ["create", "../repo.ibundle"]);
    must_ibundle(&dst_dir, ["fetch", "../repo.ibundle"]);
    must_git_fsck_and_diff(&dst_dir, &src_dir)?;
    Ok(())
}

#[test]
fn standalone_but_semantically_empty() -> AResult<()> {
    let (_test_dir, src_dir, dst_dir) = setup()?;
    let mut commit_num = 0;
    make_repo_changes1(&src_dir, &mut commit_num);
    must_ibundle(&src_dir, ["create", "../repo.ibundle"]);
    must_ibundle(&dst_dir, ["fetch", "../repo.ibundle"]);
    // Empty requires `--allow-empty`.
    fail_ibundle(3, &src_dir, ["create", "../repo.ibundle", "--standalone"]);
    must_ibundle(
        &src_dir,
        ["create", "../repo.ibundle", "--standalone", "--allow-empty"],
    );
    must_ibundle(&dst_dir, ["fetch", "../repo.ibundle"]);
    must_git_fsck_and_diff(&dst_dir, &src_dir)?;
    Ok(())
}

fn make_repo_changes2(repo_path: impl AsRef<Path>, commit_num: &mut usize) {
    let repo_path = repo_path.as_ref();
    must_git_commit_file(repo_path, commit_num);
    must_git_branch_delete(repo_path, "branch1");
    must_git_branch(repo_path, "main2", "HEAD");
    must_git_commit_file(repo_path, commit_num);
    must_git_tag_delete(repo_path, "tag1");
    must_git_tag(repo_path, "tag2", "HEAD");
    must_git_atag(repo_path, "atag2", "HEAD");
    must_git_commit_file(repo_path, commit_num);
    must_git_commit_file(repo_path, commit_num);
}

#[test]
fn two_changes() -> AResult<()> {
    let (_test_dir, src_dir, dst_dir) = setup()?;
    let mut commit_num = 0;
    make_repo_changes1(&src_dir, &mut commit_num);
    must_ibundle(&src_dir, ["create", "../repo.ibundle"]);
    must_ibundle(&dst_dir, ["fetch", "../repo.ibundle"]);
    make_repo_changes2(&src_dir, &mut commit_num);
    must_ibundle(&src_dir, ["create", "../repo.ibundle"]);
    must_ibundle(&dst_dir, ["fetch", "../repo.ibundle"]);
    must_git_fsck_and_diff(&dst_dir, &src_dir)?;
    Ok(())
}

#[test]
fn wrong_repo_id() -> AResult<()> {
    let (_test_dir, src_dir, dst_dir) = setup()?;
    must_ibundle(&src_dir, ["create", "../repo.ibundle"]);
    must_ibundle(&dst_dir, ["fetch", "../repo.ibundle"]);
    fs::write(
        &dst_dir.join("ibundle").join("id"),
        b"00000000-0000-0000-0000-000000000000",
    )
    .unwrap();
    fail_ibundle(1, &dst_dir, ["fetch", "../repo.ibundle"]);
    Ok(())
}

#[test]
fn checkout_branch_and_commit() -> AResult<()> {
    let (_test_dir, src_dir, dst_dir) = setup()?;
    let mut commit_num = 0;
    make_repo_changes1(&src_dir, &mut commit_num);
    must_ibundle(&src_dir, ["create", "../repo.ibundle"]);
    must_ibundle(&dst_dir, ["fetch", "../repo.ibundle"]);
    must_git_checkout(&src_dir, "branch1");
    must_git_commit_file(&src_dir, &mut commit_num);
    must_ibundle(&src_dir, ["create", "../repo.ibundle"]);
    must_ibundle(&dst_dir, ["fetch", "../repo.ibundle"]);
    must_git_fsck_and_diff(&dst_dir, &src_dir)?;
    Ok(())
}

#[test]
fn checkout_as_only_change() -> AResult<()> {
    let (_test_dir, src_dir, dst_dir) = setup()?;
    let mut commit_num = 0;
    make_repo_changes1(&src_dir, &mut commit_num);
    must_ibundle(&src_dir, ["create", "../repo.ibundle"]);
    must_ibundle(&dst_dir, ["fetch", "../repo.ibundle"]);
    must_git_checkout(&src_dir, "branch1");
    must_ibundle(&src_dir, ["create", "../repo.ibundle"]);
    must_ibundle(&dst_dir, ["fetch", "../repo.ibundle"]);
    must_git_fsck_and_diff(&dst_dir, &src_dir)?;
    Ok(())
}

#[test]
fn checkout_detached_head() -> AResult<()> {
    let (_test_dir, src_dir, dst_dir) = setup()?;
    let mut commit_num = 0;
    make_repo_changes1(&src_dir, &mut commit_num);
    must_ibundle(&src_dir, ["create", "../repo.ibundle"]);
    must_ibundle(&dst_dir, ["fetch", "../repo.ibundle"]);
    must_git_checkout(&src_dir, "HEAD~");
    must_ibundle(&src_dir, ["create", "../repo.ibundle"]);
    must_ibundle(&dst_dir, ["fetch", "../repo.ibundle"]);
    must_git_fsck_and_diff(&dst_dir, &src_dir)?;
    Ok(())
}

#[test]
fn new_commit_with_no_branch() -> AResult<()> {
    let (_test_dir, src_dir, dst_dir) = setup()?;
    let mut commit_num = 0;
    make_repo_changes1(&src_dir, &mut commit_num);
    must_ibundle(&src_dir, ["create", "../repo.ibundle"]);
    must_ibundle(&dst_dir, ["fetch", "../repo.ibundle"]);
    must_git_checkout(&src_dir, "tag1");
    must_git_commit_file(&src_dir, &mut commit_num);
    must_ibundle(&src_dir, ["create", "../repo.ibundle"]);
    must_ibundle(&dst_dir, ["fetch", "../repo.ibundle"]);
    must_git_fsck_and_diff(&dst_dir, &src_dir)?;
    Ok(())
}

#[test]
fn bundle_from_basis() -> AResult<()> {
    let (_test_dir, src_dir, dst_dir) = setup()?;
    let mut commit_num = 0;
    must_git_commit_file(&src_dir, &mut commit_num);
    must_ibundle(&src_dir, ["create", "../repo.ibundle"]);
    must_ibundle(&dst_dir, ["fetch", "../repo.ibundle"]);
    must_git_commit_file(&src_dir, &mut commit_num);
    must_ibundle(&src_dir, ["create", "../repo.ibundle"]);
    must_git_commit_file(&src_dir, &mut commit_num);
    must_ibundle(&src_dir, ["create", "../repo.ibundle", "--basis", "1"]);
    must_ibundle(&dst_dir, ["fetch", "../repo.ibundle"]);
    must_git_fsck_and_diff(&dst_dir, &src_dir)?;
    Ok(())
}

#[test]
fn bundle_from_basis_0() -> AResult<()> {
    let (_test_dir, src_dir, dst_dir) = setup()?;
    let mut commit_num = 0;
    must_git_commit_file(&src_dir, &mut commit_num);
    must_ibundle(&src_dir, ["create", "../repo.ibundle"]);
    assert_eq!(must_ibundle_status(&src_dir).max_seq_num, 1);
    must_git_commit_file(&src_dir, &mut commit_num);
    must_ibundle(&src_dir, ["create", "../repo.ibundle"]);
    assert_eq!(must_ibundle_status(&src_dir).max_seq_num, 2);
    must_git_commit_file(&src_dir, &mut commit_num);
    must_ibundle(&src_dir, ["create", "../repo.ibundle"]);
    assert_eq!(must_ibundle_status(&src_dir).max_seq_num, 3);
    must_ibundle(&src_dir, ["create", "../repo.ibundle", "--basis", "0"]);
    assert_eq!(must_ibundle_status(&src_dir).max_seq_num, 4);
    must_ibundle(&dst_dir, ["fetch", "../repo.ibundle"]);
    assert_eq!(must_ibundle_status(&dst_dir).max_seq_num, 4);
    must_git_fsck_and_diff(&dst_dir, &src_dir)?;
    Ok(())
}

#[test]
fn restart_from_basis_current() -> AResult<()> {
    let (_test_dir, src_dir, dst_dir) = setup()?;
    let mut commit_num = 0;
    make_repo_changes1(&src_dir, &mut commit_num);
    must_ibundle(&src_dir, ["create", "../repo.ibundle"]);
    must_ibundle(&dst_dir, ["fetch", "../repo.ibundle"]);
    must_git_commit_file(&src_dir, &mut commit_num);
    must_ibundle(&src_dir, ["create", "../repo.ibundle"]);
    must_ibundle(&dst_dir, ["fetch", "../repo.ibundle"]);
    fs::remove_dir_all(&dst_dir.join("ibundle"))?;
    must_ibundle(&src_dir, ["create", "../repo.ibundle", "--basis-current"]);
    fail_ibundle(1, &dst_dir, ["fetch", "../repo.ibundle"]);
    must_ibundle(&dst_dir, ["fetch", "../repo.ibundle", "--force"]);
    must_git_fsck_and_diff(&dst_dir, &src_dir)?;
    Ok(())
}

#[test]
fn squash_commits() -> AResult<()> {
    let (_test_dir, src_dir, dst_dir) = setup()?;
    let mut commit_num = 0;
    must_git_commit_file(&src_dir, &mut commit_num);
    must_git_commit_file(&src_dir, &mut commit_num);
    must_git_commit_file(&src_dir, &mut commit_num);
    must_git_commit_file(&src_dir, &mut commit_num);
    must_git_branch(&src_dir, "branch1", "HEAD");
    must_ibundle(&src_dir, ["create", "../repo.ibundle"]);
    must_ibundle(&dst_dir, ["fetch", "../repo.ibundle"]);
    must_git_branch_delete(&src_dir, "branch1");
    must_git(&src_dir, ["reset", "HEAD~2"]);
    must_git(&src_dir, ["commit", "-am", "Squash two commits."]);
    must_git(&src_dir, ["gc", "--prune=now"]);
    must_ibundle(&src_dir, ["create", "../repo.ibundle"]);
    must_ibundle(&dst_dir, ["fetch", "../repo.ibundle"]);
    must_git_fsck_and_diff(&dst_dir, &src_dir)?;
    Ok(())
}

#[test]
fn add_tags_into_past() -> AResult<()> {
    let (_test_dir, src_dir, dst_dir) = setup()?;
    let mut commit_num = 0;
    make_repo_changes1(&src_dir, &mut commit_num);
    must_git_commit_file(&src_dir, &mut commit_num);
    must_git_commit_file(&src_dir, &mut commit_num);
    must_git_commit_file(&src_dir, &mut commit_num);
    must_git_commit_file(&src_dir, &mut commit_num);
    must_ibundle(&src_dir, ["create", "../repo.ibundle"]);
    must_ibundle(&dst_dir, ["fetch", "../repo.ibundle"]);
    must_git_atag(&src_dir, "ahead-1", "HEAD~1");
    must_git_atag(&src_dir, "ahead-2", "HEAD~2");
    must_git_atag(&src_dir, "ahead-3", "HEAD~3");
    must_git_atag(&src_dir, "ahead-4", "HEAD~4");
    must_ibundle(&src_dir, ["create", "../repo.ibundle"]);
    must_ibundle(&dst_dir, ["fetch", "../repo.ibundle"]);
    must_git_fsck_and_diff(&dst_dir, &src_dir)?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn non_utf8_branch() -> AResult<()> {
    let (_test_dir, src_dir, dst_dir) = setup()?;
    let mut commit_num = 0;
    must_git_commit_file(&src_dir, &mut commit_num);
    must_git_branch(&src_dir, B(b"branch_non_utf8\x80"), "HEAD");
    must_ibundle(&src_dir, ["create", "../repo.ibundle"]);
    must_ibundle(&dst_dir, ["fetch", "../repo.ibundle"]);
    must_git_fsck_and_diff(&dst_dir, &src_dir)?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn checkout_non_utf8_branch() -> AResult<()> {
    let (_test_dir, src_dir, dst_dir) = setup()?;
    let mut commit_num = 0;
    let branch_non_utf8 = B(b"branch_non_utf8\x80");
    must_git_commit_file(&src_dir, &mut commit_num);
    must_git_branch(&src_dir, branch_non_utf8, "HEAD");
    must_ibundle(&src_dir, ["create", "../repo.ibundle"]);
    must_ibundle(&dst_dir, ["fetch", "../repo.ibundle"]);
    must_git_fsck_and_diff(&dst_dir, &src_dir)?;
    must_git_checkout(&src_dir, branch_non_utf8);
    must_ibundle(&src_dir, ["create", "../repo.ibundle"]);
    must_ibundle(&dst_dir, ["fetch", "../repo.ibundle"]);
    must_git_fsck_and_diff(&dst_dir, &src_dir)?;
    Ok(())
}
