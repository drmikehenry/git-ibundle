use std::collections;
use std::ffi;
use std::fs;
use std::io;
use std::io::Write;
use std::path;
use tempfile;
use uuid;

use anyhow::{anyhow, bail, Context};
use bstr::{BStr, BString, ByteSlice};

use clap::Parser;

type AResult<T> = anyhow::Result<T>;
type SeqNum = u64;
type SeqNums = Vec<SeqNum>;

const STATUS_OK: i32 = 0;
const STATUS_ERROR: i32 = 1;
const STATUS_EMPTY_BUNDLE: i32 = 3;

const IBUNDLE_FORMAT_V1: &[u8] = b"# v1 git ibundle";
const REPO_META_FORMAT_V1: &[u8] = b"# v1 repo meta";
const GIT_BUNDLE_FORMAT_V2: &[u8] = b"# v2 git bundle";

fn quoted_bstr(s: &BStr) -> String {
    if s.is_ascii() && !s.contains(&b'\'') {
        format!("'{}'", s)
    } else {
        format!("{:?}", s)
    }
}

fn quoted<B: AsRef<BStr>>(s: B) -> String {
    quoted_bstr(s.as_ref())
}

fn quoted_path<P: AsRef<std::path::Path>>(path: P) -> String {
    let p = path.as_ref().display().to_string();
    quoted(p.as_bytes())
}

fn name_to_string(name: &BStr) -> AResult<String> {
    if let Ok(s) = name.to_str() {
        Ok(s.to_string())
    } else {
        bail!("name {} is not valid UTF8", quoted_bstr(name));
    }
}

fn open_file<P: AsRef<std::path::Path>>(path: P) -> AResult<fs::File> {
    let path = path.as_ref();
    fs::File::open(path).with_context(|| {
        format!("failed to open file {} for reading", quoted_path(path))
    })
}

fn open_reader<P: AsRef<std::path::Path>>(
    path: P,
) -> AResult<io::BufReader<fs::File>> {
    Ok(io::BufReader::new(open_file(path)?))
}

fn create_file<P: AsRef<std::path::Path>>(path: P) -> AResult<fs::File> {
    let path = path.as_ref();
    fs::File::create(path).with_context(|| {
        format!("failed to create file {} for writing", quoted_path(path))
    })
}

fn create_writer<P: AsRef<std::path::Path>>(
    path: P,
) -> AResult<io::BufWriter<fs::File>> {
    Ok(io::BufWriter::new(create_file(path)?))
}

fn read_line_bytes(
    f: &mut impl io::BufRead,
    line: &mut BString,
) -> AResult<bool> {
    line.clear();
    f.read_until(b'\n', line)?;
    if line.ends_with(&[b'\n']) {
        line.pop();
    }
    Ok(line.len() > 0)
}

fn bstr_pop_word<'a>(bstr: &'a BStr) -> (&'a BStr, &'a BStr) {
    if let Some((word, rest)) = bstr.split_once_str(b" ") {
        (word.as_bstr(), rest.as_bstr())
    } else {
        (bstr, &bstr[bstr.len()..])
    }
}

fn parse_bool(bstr: &BStr) -> AResult<bool> {
    if bstr == b"true".as_bstr() {
        Ok(true)
    } else if bstr == b"false".as_bstr() {
        Ok(false)
    } else {
        bail!("invalid boolean {}", bstr);
    }
}

fn bool_as_bstr(value: bool) -> &'static BStr {
    if value {
        b"true".as_bstr()
    } else {
        b"false".as_bstr()
    }
}

fn parse_seq_num<S: AsRef<[u8]>>(bstr: S) -> AResult<SeqNum> {
    Ok(bstr.as_ref().to_str_lossy().parse::<SeqNum>()?)
}

fn oid_to_bstring(oid: &git2::Oid) -> BString {
    oid.to_string().into()
}

fn write_oid_bstr_bline<T: io::Write>(
    f: &mut T,
    oid: &git2::Oid,
    bstr: &BStr,
) -> AResult<()> {
    f.write_all(oid.to_string().as_bytes())?;
    f.write_all(b" ")?;
    f.write_all(bstr)?;
    f.write_all(b"\n")?;
    Ok(())
}

fn parse_oid(bstr: &BStr) -> AResult<git2::Oid> {
    Ok(git2::Oid::from_str(std::str::from_utf8(bstr)?)?)
}

fn oid_bstr_parse(bstr: &BStr) -> AResult<(git2::Oid, BString)> {
    if bstr.find_byte(b' ').is_some() {
        let (oid_bstr, rest_bstr) = bstr_pop_word(bstr);
        Ok((parse_oid(oid_bstr)?, BString::from(rest_bstr)))
    } else {
        bail!("missing space in {}", bstr);
    }
}

//////////////////////////////////////////////////////////////////////////////

/// Git offline incremental mirroring via ibundle files
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
#[command(propagate_version = true)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(clap::Args, Debug)]
struct CreateArgs {
    /// ibundle file to create
    #[arg(value_name = "IBUNDLE_FILE")]
    ibundle_path: path::PathBuf,

    /// force ibundle to be standalone
    #[arg(long)]
    standalone: bool,

    /// choose alternate basis sequence number
    #[arg(long)]
    basis: Option<SeqNum>,

    /// run quietly
    #[arg(short, long)]
    quiet: bool,

    /// allow creation of an empty ibundle
    #[arg(long)]
    allow_empty: bool,
}

#[derive(clap::Args, Debug)]
struct FetchArgs {
    /// ibundle file to fetch
    #[arg(value_name = "IBUNDLE_FILE")]
    ibundle_path: path::PathBuf,

    /// perform a trial fetch without making changes to the repository
    #[arg(long)]
    dry_run: bool,

    /// run quietly
    #[arg(short, long)]
    quiet: bool,

    /// force fetch operation
    #[arg(long)]
    force: bool,
}

#[derive(clap::Args, Debug)]
struct ToBundleArgs {
    /// ibundle file to convert
    #[arg(value_name = "IBUNDLE_FILE")]
    ibundle_path: path::PathBuf,

    /// bundle file to create from ibundle
    #[arg(value_name = "BUNDLE_FILE")]
    bundle_path: path::PathBuf,

    /// run quietly
    #[arg(short, long)]
    quiet: bool,

    /// force fetch operation
    #[arg(long)]
    force: bool,
}

#[derive(clap::Args, Debug)]
struct StatusArgs {
    /// Provide longer status
    #[arg(long)]
    long: bool,
}

#[derive(clap::Args, Debug)]
struct CleanArgs {
    /// Number of sequence numbers to retain
    #[arg(long, default_value = "20")]
    keep: usize,
}

#[derive(clap::Subcommand, Debug)]
enum Commands {
    /// Create an ibundle
    Create(CreateArgs),

    /// Fetch from an ibundle
    Fetch(FetchArgs),

    /// Convert an ibundle into a bundle
    ToBundle(ToBundleArgs),

    /// Report status
    Status(StatusArgs),

    /// Cleanup old sequence numbers
    Clean(CleanArgs),
}

//////////////////////////////////////////////////////////////////////////////

type RefName = BString;

fn ref_names_write<W: io::Write>(
    ref_names: &Vec<RefName>,
    writer: &mut W,
) -> AResult<()> {
    for name in ref_names.iter() {
        writer.write_all(name)?;
        writer.write_all(b"\n")?;
    }
    writer.write_all(b".\n")?;
    Ok(())
}

fn ref_names_read<R: io::BufRead>(reader: &mut R) -> AResult<Vec<RefName>> {
    let mut ref_names = Vec::new();
    let mut line = RefName::from("");
    while read_line_bytes(reader, &mut line)? {
        if line == "." {
            return Ok(ref_names);
        }
        ref_names.push(line.clone());
    }
    bail!("ref_names: missing final '.'; got {}", quoted(line));
}

// `name` => `Oid`.
type ORefs = collections::BTreeMap<RefName, git2::Oid>;

fn orefs_write<W: io::Write>(orefs: &ORefs, writer: &mut W) -> AResult<()> {
    for (name, oid) in orefs.iter() {
        write_oid_bstr_bline(writer, oid, name.as_bstr())?;
    }
    writer.write_all(b".\n")?;
    Ok(())
}

fn orefs_read<R: io::BufRead>(reader: &mut R) -> AResult<ORefs> {
    let mut orefs = ORefs::new();
    let mut line = BString::from("");
    while read_line_bytes(reader, &mut line)? {
        if line == "." {
            return Ok(orefs);
        }
        let (oid, name) = oid_bstr_parse(line.as_bstr())?;
        orefs.insert(name, oid);
    }
    bail!("orefs: missing final '.'; got {}", quoted(line));
}

type Commits = collections::BTreeMap<git2::Oid, BString>;

fn commits_write<W: io::Write>(
    commits: &Commits,
    writer: &mut W,
) -> AResult<()> {
    for (oid, comment) in commits.iter() {
        write_oid_bstr_bline(writer, oid, comment.as_bstr())?;
    }
    writer.write_all(b".\n")?;
    Ok(())
}

fn commits_read<R: io::BufRead>(reader: &mut R) -> AResult<Commits> {
    let mut commits = Commits::new();
    let mut line = BString::from("");
    while read_line_bytes(reader, &mut line)? {
        if line == "." {
            return Ok(commits);
        }
        let (oid, comment) = oid_bstr_parse(line.as_bstr())?;
        commits.insert(oid, comment);
    }
    bail!("commits: missing final '.'; got {}", quoted(line));
}

fn repo_open<P: AsRef<std::path::Path>>(
    repo_path: P,
) -> AResult<git2::Repository> {
    let repo_path = repo_path.as_ref();
    let repo = match git2::Repository::open(repo_path) {
        Ok(repo) => repo,
        Err(_) => {
            bail!(
                "could not open Git repository at {}",
                quoted_path(repo_path)
            );
        }
    };
    Ok(repo)
}

fn repo_state_root_path(repo: &git2::Repository) -> path::PathBuf {
    repo.path().join("ibundle")
}

fn repo_temp_dir_path(repo: &git2::Repository) -> path::PathBuf {
    repo_state_root_path(repo).join("temp")
}

fn repo_mktemp(repo: &git2::Repository) -> AResult<path::PathBuf> {
    let temp_dir_path = repo_temp_dir_path(repo);
    fs::create_dir_all(&temp_dir_path)?;
    Ok(temp_dir_path)
}

fn repo_meta_dir_path(repo: &git2::Repository) -> path::PathBuf {
    repo_state_root_path(repo).join("repo_meta")
}

fn repo_meta_path(repo: &git2::Repository, seq_num: SeqNum) -> path::PathBuf {
    repo_meta_dir_path(&repo).join(&seq_num.to_string())
}

fn repo_id_path(repo: &git2::Repository) -> path::PathBuf {
    repo_state_root_path(repo).join("id")
}

fn repo_orefs(repo: &git2::Repository) -> AResult<ORefs> {
    let mut orefs = ORefs::new();
    for r in repo.references()? {
        let r = r?;
        let oid = if let Some(oid) = r.target() {
            oid
        } else {
            bail!("found non-direct ref kind {:?}", r.kind());
        };
        let name = RefName::from(r.name_bytes());
        orefs.insert(name, oid);
    }
    Ok(orefs)
}

fn repo_is_empty(repo: &git2::Repository) -> AResult<bool> {
    let orefs = repo_orefs(repo)?;
    Ok(orefs.len() == 0)
}

fn repo_find_missing_commits(
    repo: &git2::Repository,
    commits: &Commits,
) -> Commits {
    commits
        .iter()
        .filter_map(|(&commit_id, comment)| {
            if !repo.find_commit(commit_id).is_ok() {
                Some((commit_id, comment.clone()))
            } else {
                None
            }
        })
        .collect()
}

fn repo_find_valid_commits(
    repo: &git2::Repository,
    commits: &Commits,
) -> Commits {
    commits
        .iter()
        .filter_map(|(&commit_id, comment)| {
            if repo.find_commit(commit_id).is_ok() {
                Some((commit_id, comment.clone()))
            } else {
                None
            }
        })
        .collect()
}

//////////////////////////////////////////////////////////////////////////////

fn pack_push_oid(pack: &mut BString, oid: &git2::Oid) {
    pack.extend_from_slice(&oid.to_string().as_bytes());
    pack.push(b'\n');
}

fn pack_push_basis_oid(pack: &mut BString, basis_oid: &git2::Oid) {
    pack.push(b'^');
    pack_push_oid(pack, basis_oid);
}

fn pack_objects_into(
    pack: &BString,
    pack_file: fs::File,
    quiet: bool,
) -> AResult<()> {
    let mut args =
        vec!["pack-objects", "--stdout", "--thin", "--delta-base-offset"];
    if quiet {
        args.push("--quiet")
    }
    let mut child = std::process::Command::new("git")
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(pack_file)
        .spawn()?;

    let mut child_stdin = child.stdin.take().unwrap();
    child_stdin.write_all(&pack)?;
    drop(child_stdin);

    let exit_status = child.wait()?;
    if !exit_status.success() {
        bail!("failure in git pack-objects");
    }
    Ok(())
}

//////////////////////////////////////////////////////////////////////////////

struct Directive {}
impl Directive {
    const REPO_ID: &[u8] = b"repo_id";
    const SEQ_NUM: &[u8] = b"seq_num";
    const BASIS_SEQ_NUM: &[u8] = b"basis_seq_num";
    const STANDALONE: &[u8] = b"standalone";
    const HEAD_REF: &[u8] = b"head_ref";
    const HEAD_DETACHED: &[u8] = b"head_detached";
    const COMMITS: &[u8] = b"commits";
    const PREREQS: &[u8] = b"prereqs";
    const CHANGED_OREFS: &[u8] = b"changed_orefs";
    const REMOVED_REFS: &[u8] = b"removed_refs";
    const OREFS: &[u8] = b"orefs";
}

fn write_directive<W: io::Write, D: AsRef<[u8]>, Rest: AsRef<[u8]>>(
    writer: &mut W,
    directive: D,
    rest: Rest,
) -> AResult<()> {
    writer.write_all(b"%")?;
    writer.write_all(directive.as_ref())?;
    writer.write_all(b" ")?;
    writer.write_all(rest.as_ref())?;
    writer.write_all(b"\n")?;
    Ok(())
}

fn write_directive_bool<W: io::Write, D: AsRef<[u8]>>(
    writer: &mut W,
    directive: D,
    value: bool,
) -> AResult<()> {
    let value_bstr = bool_as_bstr(value);
    writer.write_all(b"%")?;
    writer.write_all(directive.as_ref())?;
    writer.write_all(b" ")?;
    writer.write_all(value_bstr)?;
    writer.write_all(b"\n")?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RepoMeta {
    head_ref: BString,
    head_detached: bool,
    orefs: ORefs,
    commits: Commits,
}

impl RepoMeta {
    fn new() -> Self {
        Self {
            head_ref: BString::from(""),
            head_detached: false,
            orefs: ORefs::new(),
            commits: Commits::new(),
        }
    }

    fn read<R: io::BufRead>(reader: &mut R) -> AResult<Self> {
        let mut line = BString::from("");
        read_line_bytes(reader, &mut line)?;
        if line != REPO_META_FORMAT_V1 {
            bail!("invalid repo meta file");
        }

        let mut meta = Self::new();
        while read_line_bytes(reader, &mut line)? {
            if line.starts_with(b"%") {
                let (dir, rest) = bstr_pop_word(line[1..].as_bstr());
                if dir == Directive::HEAD_REF {
                    meta.head_ref = BString::from(rest);
                } else if dir == Directive::HEAD_DETACHED {
                    meta.head_detached = parse_bool(rest.as_bstr())?;
                } else if dir == Directive::COMMITS {
                    meta.commits = commits_read(reader)?;
                } else if dir == Directive::OREFS {
                    meta.orefs = orefs_read(reader)?;
                } else {
                    bail!("invalid RepoMeta directive {}", line);
                }
            } else {
                bail!("invalid RepoMeta line {}", line);
            }
        }
        Ok(meta)
    }

    fn write<W: io::Write>(&self, writer: &mut W) -> AResult<()> {
        writer.write_all(REPO_META_FORMAT_V1)?;
        writer.write_all(b"\n")?;
        write_directive(writer, Directive::HEAD_REF, &self.head_ref)?;
        write_directive_bool(
            writer,
            Directive::HEAD_DETACHED,
            self.head_detached,
        )?;
        write_directive(writer, Directive::COMMITS, "")?;
        commits_write(&self.commits, writer)?;
        write_directive(writer, Directive::OREFS, "")?;
        orefs_write(&self.orefs, writer)?;
        writer.write_all(b"\n")?;
        Ok(())
    }
}

fn git_bundle_header_write<W: io::Write>(
    writer: &mut W,
    prereqs: &Commits,
    orefs: &ORefs,
) -> AResult<()> {
    writer.write_all(GIT_BUNDLE_FORMAT_V2)?;
    writer.write_all(b"\n")?;
    for (commit_id, comment) in prereqs.iter() {
        writer.write_all(b"-")?;
        write_oid_bstr_bline(writer, commit_id, comment.as_bstr())?;
    }
    for (name, oid) in orefs.iter() {
        write_oid_bstr_bline(writer, oid, name.as_bstr())?;
    }
    writer.write_all(b"\n")?;
    Ok(())
}

fn git_fetch_bundle(
    bundle_path: &path::Path,
    quiet: bool,
    dry_run: bool,
) -> AResult<()> {
    let mut args: Vec<ffi::OsString> = vec!["fetch".into(), "--prune".into()];
    if quiet {
        args.push("-q".into())
    }
    if dry_run {
        args.push("--dry-run".into())
    }
    args.push(bundle_path.as_os_str().into());
    args.push("*:*".into());
    let mut child = std::process::Command::new("git").args(args).spawn()?;
    let exit_status = child.wait()?;
    if !exit_status.success() {
        bail!("failure in git fetch");
    }
    Ok(())
}

fn git_fetch_from_pack<R: io::Read>(
    repo: &git2::Repository,
    prereqs: &Commits,
    orefs: &ORefs,
    pack_reader: R,
    quiet: bool,
    dry_run: bool,
) -> AResult<()> {
    let temp_dir_path = repo_mktemp(&repo)?;
    let mut tmp = tempfile::Builder::new()
        .prefix("temp")
        .suffix(".bundle")
        .tempfile_in(&temp_dir_path)?;
    let mut tmp_file = tmp.as_file_mut();

    git_bundle_header_write(&mut tmp_file, prereqs, orefs)?;

    let mut pack_reader = pack_reader;
    io::copy(&mut pack_reader, tmp_file)?;
    git_fetch_bundle(&tmp.path(), quiet, dry_run)?;
    Ok(())
}

//////////////////////////////////////////////////////////////////////////////

fn repo_commit(
    repo: &git2::Repository,
    commit_id: git2::Oid,
) -> AResult<git2::Commit> {
    Ok(repo.find_object(commit_id, None)?.peel_to_commit()?)
}

fn commit_comment(commit: &git2::Commit) -> BString {
    let comment = if let Some(summary) = commit.summary_bytes() {
        BString::from(summary)
    } else {
        BString::from("")
    };
    comment
}

fn repo_commit_id_comment(
    repo: &git2::Repository,
    commit_id: git2::Oid,
) -> AResult<(git2::Oid, BString)> {
    let commit = repo_commit(repo, commit_id)?;
    Ok((commit.id(), commit_comment(&commit)))
}

fn repo_seq_nums(repo: &git2::Repository) -> AResult<SeqNums> {
    let mut seq_nums = SeqNums::new();
    let meta_dir_path = repo_meta_dir_path(&repo);
    if let Ok(dir_iter) = fs::read_dir(&meta_dir_path) {
        for entry in dir_iter {
            if let Ok(seq_num) =
                entry?.file_name().to_string_lossy().parse::<u64>()
            {
                seq_nums.push(seq_num);
            }
        }
    }
    seq_nums.sort_by(|a, b| b.cmp(a));
    Ok(seq_nums)
}

fn repo_has_basis(repo: &git2::Repository, basis_seq_num: &SeqNum) -> bool {
    if let Ok(seq_nums) = repo_seq_nums(&repo) {
        seq_nums.contains(basis_seq_num)
    } else {
        false
    }
}

fn repo_id_new() -> BString {
    BString::from(uuid::Uuid::new_v4().to_string())
}

fn repo_id_read(repo: &git2::Repository) -> Option<BString> {
    fs::read_to_string(&repo_id_path(repo))
        .ok()
        .map(|s| BString::from(s.trim_end()))
}

fn repo_id_write(repo: &git2::Repository, repo_id: &BStr) -> AResult<()> {
    fs::create_dir_all(&repo_state_root_path(repo))?;
    let mut id_bytes = BString::from(repo_id);
    id_bytes.push(b'\n');
    fs::write(&repo_id_path(repo), id_bytes)?;
    Ok(())
}

fn repo_meta_current(repo: &git2::Repository) -> AResult<RepoMeta> {
    let mut meta = RepoMeta::new();
    meta.orefs = repo_orefs(&repo)?;
    let head_ref = repo
        .find_reference("HEAD")
        .context("cannot find `HEAD` reference")?;
    meta.head_detached = repo
        .head_detached()
        .context("failed to determine detached state")?;
    if meta.head_detached {
        let head_commit_id = head_ref.target().ok_or(anyhow!(
            "cannot retrieve detached-head commit id for `HEAD`"
        ))?;
        meta.head_ref = oid_to_bstring(&head_commit_id);
    } else {
        let head_sym_target = head_ref
            .symbolic_target_bytes()
            .ok_or(anyhow!("cannot retrieve symbolic ref for `HEAD`"))?;
        meta.head_ref = BString::from(head_sym_target);
    }
    if let Ok(head_commit) = head_ref.peel_to_commit() {
        meta.orefs.insert(BString::from("HEAD"), head_commit.id());
    }
    for (_name, &oid) in meta.orefs.iter() {
        let (commit_id, comment) = repo_commit_id_comment(repo, oid)?;
        meta.commits.insert(commit_id, comment);
    }
    Ok(meta)
}

fn repo_meta_read(
    repo: &git2::Repository,
    seq_num: SeqNum,
) -> AResult<RepoMeta> {
    let meta_path = repo_meta_path(repo, seq_num);
    let mut f = open_reader(&meta_path)?;
    RepoMeta::read(&mut f)
}

fn repo_meta_write(
    repo: &git2::Repository,
    seq_num: SeqNum,
    meta: &RepoMeta,
) -> AResult<()> {
    let meta_dir_path = repo_meta_dir_path(&repo);
    fs::create_dir_all(&meta_dir_path)?;
    let meta_path = repo_meta_path(repo, seq_num);
    let mut f = create_writer(&meta_path)?;
    meta.write(&mut f)
}

//////////////////////////////////////////////////////////////////////////////

// Remove from `orefs` each matching `oref` in `orefs_to_remove`.
fn orefs_sub(orefs: &ORefs, orefs_to_remove: &ORefs) -> ORefs {
    orefs
        .iter()
        .filter_map(|(name, oid)| {
            if orefs_to_remove.get_key_value(name) == Some((name, oid)) {
                None
            } else {
                Some((name.clone(), *oid))
            }
        })
        .collect()
}

// Return names in `orefs` with names from `orefs_to_remove` removed.
fn orefs_sub_names(orefs: &ORefs, orefs_to_remove: &ORefs) -> Vec<RefName> {
    orefs
        .iter()
        .filter_map(|(name, _oid)| {
            if !orefs_to_remove.contains_key(name) {
                Some(name)
            } else {
                None
            }
        })
        .cloned()
        .collect()
}

//////////////////////////////////////////////////////////////////////////////

#[derive(Debug, Clone)]
struct IBundle {
    repo_id: BString,
    seq_num: SeqNum,
    basis_seq_num: SeqNum,
    standalone: bool,
    head_ref: BString,
    head_detached: bool,
    prereqs: Commits,
    changed_orefs: ORefs,
    removed_refs: Vec<RefName>,
    orefs: ORefs,
}

impl IBundle {
    fn construct(
        repo: &git2::Repository,
        repo_id: BString,
        seq_num: SeqNum,
        basis_seq_num: SeqNum,
        meta: &RepoMeta,
        basis_meta: &RepoMeta,
        standalone: bool,
    ) -> AResult<Self> {
        let ibundle = IBundle {
            repo_id,
            seq_num,
            basis_seq_num,
            standalone: standalone || basis_seq_num == 0,
            head_ref: meta.head_ref.clone(),
            head_detached: meta.head_detached,
            prereqs: repo_find_valid_commits(repo, &basis_meta.commits),
            changed_orefs: orefs_sub(&meta.orefs, &basis_meta.orefs),
            removed_refs: orefs_sub_names(&basis_meta.orefs, &meta.orefs),
            orefs: meta.orefs.clone(),
        };

        Ok(ibundle)
    }

    fn new() -> Self {
        Self {
            repo_id: BString::from(""),
            seq_num: 0,
            basis_seq_num: 0,
            standalone: true,
            head_ref: BString::from(""),
            head_detached: false,
            prereqs: Commits::new(),
            changed_orefs: ORefs::new(),
            removed_refs: Vec::new(),
            orefs: ORefs::new(),
        }
    }

    fn read<R: io::BufRead>(reader: &mut R) -> AResult<Self> {
        let mut line = BString::from("");
        read_line_bytes(reader, &mut line)?;
        if line != IBUNDLE_FORMAT_V1 {
            bail!("not a V1 ibundle file");
        }

        let mut meta = Self::new();
        while read_line_bytes(reader, &mut line)? {
            if line.starts_with(b"%") {
                let (dir, rest) = bstr_pop_word(line[1..].as_bstr());
                if dir == Directive::REPO_ID {
                    meta.repo_id = BString::from(rest);
                } else if dir == Directive::SEQ_NUM {
                    meta.seq_num = parse_seq_num(rest)?;
                } else if dir == Directive::BASIS_SEQ_NUM {
                    meta.basis_seq_num = parse_seq_num(rest)?;
                } else if dir == Directive::STANDALONE {
                    meta.standalone = parse_bool(rest.as_bstr())?;
                } else if dir == Directive::HEAD_REF {
                    meta.head_ref = BString::from(rest);
                } else if dir == Directive::HEAD_DETACHED {
                    meta.head_detached = parse_bool(rest.as_bstr())?;
                } else if dir == Directive::PREREQS {
                    meta.prereqs = commits_read(reader)?;
                } else if dir == Directive::CHANGED_OREFS {
                    meta.changed_orefs = orefs_read(reader)?;
                } else if dir == Directive::REMOVED_REFS {
                    meta.removed_refs = ref_names_read(reader)?;
                } else if dir == Directive::OREFS {
                    meta.orefs = orefs_read(reader)?;
                } else {
                    bail!("invalid ibundle directive {}", line);
                }
            } else {
                bail!("invalid ibundle line {}", line);
            }
        }
        if meta.standalone {
            if meta.removed_refs.len() > 0 {
                bail!("standalone ibundle has removed_refs");
            }
            if meta.changed_orefs.len() > 0 {
                bail!("standalone ibundle has changed_orefs");
            }
        } else {
            if meta.orefs.len() > 0 {
                bail!("non-standalone ibundle has orefs");
            }
            if meta.prereqs.len() > 0 {
                bail!("non-standalone ibundle has prereqs");
            }
        }
        Ok(meta)
    }

    fn write<W: io::Write>(&self, writer: &mut W) -> AResult<()> {
        writer.write_all(IBUNDLE_FORMAT_V1)?;
        writer.write_all(b"\n")?;
        write_directive(writer, Directive::REPO_ID, &self.repo_id)?;
        write_directive(
            writer,
            Directive::SEQ_NUM,
            &format!("{}", self.seq_num),
        )?;
        write_directive(
            writer,
            Directive::BASIS_SEQ_NUM,
            &format!("{}", self.basis_seq_num),
        )?;
        write_directive_bool(writer, Directive::STANDALONE, self.standalone)?;
        write_directive(writer, Directive::HEAD_REF, &self.head_ref)?;
        write_directive_bool(
            writer,
            Directive::HEAD_DETACHED,
            self.head_detached,
        )?;
        if self.standalone {
            write_directive(writer, Directive::PREREQS, "")?;
            commits_write(&self.prereqs, writer)?;
            write_directive(writer, Directive::OREFS, "")?;
            orefs_write(&self.orefs, writer)?;
        } else {
            write_directive(writer, Directive::CHANGED_OREFS, "")?;
            orefs_write(&self.changed_orefs, writer)?;
            write_directive(writer, Directive::REMOVED_REFS, "")?;
            ref_names_write(&self.removed_refs, writer)?;
        }
        writer.write_all(b"\n")?;
        Ok(())
    }

    fn write_pack(&self, pack_file: fs::File, quiet: bool) -> AResult<()> {
        let mut pack = BString::from("");
        for (commit_id, _comment) in self.prereqs.iter() {
            pack_push_basis_oid(&mut pack, commit_id);
        }
        for (_name, oid) in self.orefs.iter() {
            pack_push_oid(&mut pack, oid);
        }
        pack_objects_into(&pack, pack_file, quiet)?;
        Ok(())
    }

    fn validate_repo_identity(
        self: &Self,
        repo: &git2::Repository,
        force: bool,
    ) -> AResult<()> {
        if let Some(repo_id) = repo_id_read(&repo) {
            if repo_id != self.repo_id {
                bail!(
                    "repo's repo_id({}) != ibundle repo_id({})",
                    repo_id,
                    self.repo_id
                );
            }
        } else if !force && !repo_is_empty(repo)? {
            bail!("repo lacks repo_id and is non-empty; consider `--force`");
        }

        Ok(())
    }

    fn determine_basis_meta(
        self: &Self,
        repo: &git2::Repository,
        force: bool,
    ) -> AResult<RepoMeta> {
        let basis_meta = if self.basis_seq_num == 0 {
            RepoMeta::new()
        } else if repo_has_basis(repo, &self.basis_seq_num) {
            repo_meta_read(&repo, self.basis_seq_num)?
        } else if !self.standalone {
            bail!(
                std::concat!(
                    "repo missing basis_seq_num={} and ibundle is not ",
                    "standalone; consider `create --standalone`"
                ),
                self.basis_seq_num
            );
        } else if !force {
            bail!(
                std::concat!(
                    "repo missing basis_seq_num={}, but ibundle is ",
                    "standalone; consider `--force`",
                ),
                self.basis_seq_num
            );
        } else {
            RepoMeta::new()
        };

        Ok(basis_meta)
    }

    fn apply_basis_meta(&mut self, basis_meta: &RepoMeta) -> AResult<()> {
        if self.standalone {
            self.changed_orefs = orefs_sub(&self.orefs, &basis_meta.orefs);
            self.removed_refs = orefs_sub_names(&basis_meta.orefs, &self.orefs);
        } else {
            self.orefs = basis_meta.orefs.clone();
            for name in &self.removed_refs {
                self.orefs.remove(name);
            }
            for (name, &oid) in self.changed_orefs.iter() {
                self.orefs.insert(name.clone(), oid);
            }
            self.prereqs = basis_meta.commits.clone();
        }
        Ok(())
    }

    fn validate_and_apply_basis(
        self: &mut Self,
        repo: &git2::Repository,
        force: bool,
    ) -> AResult<()> {
        self.validate_repo_identity(repo, force)?;
        let basis_meta = self.determine_basis_meta(&repo, force)?;
        self.apply_basis_meta(&basis_meta)?;
        Ok(())
    }
}

//////////////////////////////////////////////////////////////////////////////

fn inc_seq_num(seq_num: &SeqNum) -> AResult<SeqNum> {
    let next_seq_num = match seq_num.checked_add(1) {
        Some(next_seq_num) => next_seq_num,
        None => bail!("seq_num({}) too large", seq_num),
    };
    Ok(next_seq_num)
}

fn calc_max_seq_num(seq_nums: &SeqNums) -> AResult<SeqNum> {
    let max_seq_num = if seq_nums.len() > 0 { seq_nums[0] } else { 0 };
    Ok(max_seq_num)
}

fn calc_next_seq_num(seq_nums: &SeqNums) -> AResult<SeqNum> {
    inc_seq_num(&calc_max_seq_num(seq_nums)?)
}

fn calc_basis_seq_num(
    basis_option: Option<SeqNum>,
    seq_nums: &SeqNums,
    cur_seq_num: SeqNum,
) -> AResult<SeqNum> {
    let basis_seq_num = basis_option.unwrap_or(cur_seq_num - 1);
    if basis_seq_num > 0 && !seq_nums.contains(&basis_seq_num) {
        bail!("basis not present for `--basis {}`", basis_seq_num);
    }
    Ok(basis_seq_num)
}

//////////////////////////////////////////////////////////////////////////////

fn read_ibundle<P: AsRef<std::path::Path>>(
    ibundle_path: P,
) -> AResult<(IBundle, io::BufReader<fs::File>)> {
    let ibundle_path = ibundle_path.as_ref();
    let mut ibundle_reader = open_reader(ibundle_path)?;
    let ibundle = IBundle::read(&mut ibundle_reader).with_context(|| {
        format!("failure reading ibundle file {}", quoted_path(ibundle_path))
    })?;

    Ok((ibundle, ibundle_reader))
}

fn cmd_create(create_args: &CreateArgs) -> AResult<i32> {
    let repo_path = ".";
    let repo = repo_open(repo_path)?;
    let repo_id = if let Some(repo_id) = repo_id_read(&repo) {
        repo_id
    } else {
        let repo_id = repo_id_new();
        repo_id_write(&repo, repo_id.as_bstr())?;
        repo_id
    };

    let seq_nums = repo_seq_nums(&repo)?;
    let seq_num = calc_next_seq_num(&seq_nums)?;
    let basis_seq_num =
        calc_basis_seq_num(create_args.basis, &seq_nums, seq_num)?;
    let basis_meta = if basis_seq_num > 0 {
        repo_meta_read(&repo, basis_seq_num)?
    } else {
        RepoMeta::new()
    };

    let meta = repo_meta_current(&repo)?;

    let ibundle = IBundle::construct(
        &repo,
        repo_id,
        seq_num,
        basis_seq_num,
        &meta,
        &basis_meta,
        create_args.standalone,
    )?;

    if meta == basis_meta && !create_args.allow_empty {
        eprintln!(std::concat!(
            "error: refusing to create an empty bundle; ",
            "consider `--allow-empty`"
        ));
        return Ok(STATUS_EMPTY_BUNDLE);
    }

    let mut ibundle_writer = create_writer(&create_args.ibundle_path)?;
    ibundle.write(&mut ibundle_writer)?;
    ibundle.write_pack(ibundle_writer.into_inner()?, create_args.quiet)?;

    repo_meta_write(&repo, seq_num, &meta)?;
    if !create_args.quiet {
        println!(
            "wrote {}, seq_num={}, {}/{} refs",
            quoted_path(&create_args.ibundle_path),
            ibundle.seq_num,
            ibundle.orefs.len(),
            meta.orefs.len(),
        );
    }
    Ok(STATUS_OK)
}

fn cmd_fetch(fetch_args: &FetchArgs) -> AResult<i32> {
    if !fetch_args.quiet && fetch_args.dry_run {
        println!("(dry run)");
    }
    let ibundle_path = &fetch_args.ibundle_path;
    let (mut ibundle, ibundle_reader) = read_ibundle(ibundle_path)?;

    if !fetch_args.quiet {
        println!(
            "read {}, seq_num={}, {} refs",
            quoted_path(&fetch_args.ibundle_path),
            ibundle.seq_num,
            ibundle.orefs.len()
        );
    }

    let repo_path = ".";
    let repo = repo_open(repo_path)?;

    if !repo.is_bare() {
        bail!("cannot fetch into non-bare repository");
    }

    ibundle.validate_and_apply_basis(&repo, fetch_args.force)?;

    let missing_prereqs = repo_find_missing_commits(&repo, &ibundle.prereqs);
    if missing_prereqs.len() > 0 {
        bail!(
            "repo is missing {} prerequisite commits listed in ibundle",
            missing_prereqs.len()
        );
    }

    if !fetch_args.dry_run {
        repo_id_write(&repo, ibundle.repo_id.as_bstr())?;
    }
    git_fetch_from_pack(
        &repo,
        &ibundle.prereqs,
        &ibundle.orefs,
        ibundle_reader,
        fetch_args.quiet,
        fetch_args.dry_run,
    )?;

    let head_ref = ibundle.head_ref.as_bstr();
    let meta;

    if fetch_args.dry_run {
        meta = RepoMeta {
            head_ref: BString::from(head_ref),
            head_detached: ibundle.head_detached,
            orefs: ibundle.orefs.clone(),
            commits: Commits::new(),
        };
    } else {
        if ibundle.head_detached {
            let commit_id = parse_oid(head_ref)?;
            repo.set_head_detached(commit_id)?;
        } else if head_ref != "" {
            // TODO: `name_to_string` is necessary only because git2 does not
            // provide a bytes-only way to set references.  Consider extending
            // git2 with bytes-only equivalent for `repo.set_head()`.
            repo.set_head(&name_to_string(head_ref)?)?;
        }

        meta = repo_meta_current(&repo)?;
    }

    if meta.orefs != ibundle.orefs {
        bail!("final repository refs do not match those in ibundle");
    }
    if meta.head_ref != ibundle.head_ref
        || meta.head_detached != ibundle.head_detached
    {
        bail!(
            "repository HEAD ({}{}) does not match ibundle HEAD ({}{})",
            quoted(meta.head_ref),
            if meta.head_detached { ", detached" } else { "" },
            quoted(ibundle.head_ref),
            if ibundle.head_detached {
                ", detached"
            } else {
                ""
            },
        );
    }

    if !fetch_args.dry_run {
        repo_meta_write(&repo, ibundle.seq_num, &meta)?;
    }

    if !fetch_args.quiet {
        println!(
            "final state: {} refs, HEAD {}{}",
            meta.orefs.len(),
            quoted(&meta.head_ref),
            if meta.head_detached {
                " (detached)"
            } else {
                ""
            }
        );
    }
    Ok(STATUS_OK)
}

fn cmd_to_bundle(to_bundle_args: &ToBundleArgs) -> AResult<i32> {
    let ibundle_path = &to_bundle_args.ibundle_path;
    let (mut ibundle, mut ibundle_reader) = read_ibundle(ibundle_path)?;
    if !to_bundle_args.quiet {
        println!(
            "read {}, seq_num={}, {} refs",
            quoted_path(&to_bundle_args.ibundle_path),
            ibundle.seq_num,
            ibundle.orefs.len()
        );
    }

    if !ibundle.standalone {
        let repo_path = ".";
        let repo = repo_open(repo_path)?;
        ibundle.validate_and_apply_basis(&repo, to_bundle_args.force)?;
    };

    let mut writer = create_writer(&to_bundle_args.bundle_path)?;
    git_bundle_header_write(&mut writer, &ibundle.prereqs, &ibundle.orefs)?;

    io::copy(&mut ibundle_reader, &mut writer)?;
    if !to_bundle_args.quiet {
        println!(
            "wrote {}, {} refs, {} prereqs",
            quoted_path(&to_bundle_args.bundle_path),
            ibundle.orefs.len(),
            ibundle.prereqs.len(),
        );
        println!("To apply this bundle file in destination repository:");
        println!("  git fetch .../file.bundle --prune \"*:*\"");
        if ibundle.head_detached {
            println!("  git update-ref --no-deref HEAD {}", ibundle.head_ref);
        } else {
            println!("  git symbolic-ref HEAD {}", ibundle.head_ref);
        }
    }
    Ok(STATUS_OK)
}

fn cmd_status(status_args: &StatusArgs) -> AResult<i32> {
    let repo_path = ".";
    let repo = repo_open(repo_path)?;
    let mut failed = false;

    let repo_id = repo_id_read(&repo).unwrap_or(BString::from("NONE"));
    let seq_nums = repo_seq_nums(&repo)?;
    let max_seq_num = calc_max_seq_num(&seq_nums)?;
    let next_seq_num = calc_next_seq_num(&seq_nums)?;

    println!("repo_id: {}", repo_id);
    println!("max_seq_num: {}", max_seq_num);
    println!("next_seq_num: {}", next_seq_num);

    if seq_nums.len() > 0 {
        if status_args.long {
            println!("long_details:");
            println!("  {:<8} {:<8} {}", "seq_num", "num_refs", "HEAD");
            for &seq_num in seq_nums.iter().rev() {
                match repo_meta_read(&repo, seq_num) {
                    Ok(meta) => {
                        println!(
                            "  {:<8} {:<8} {}{}",
                            seq_num,
                            meta.orefs.len(),
                            meta.head_ref,
                            if meta.head_detached {
                                " (detached)"
                            } else {
                                ""
                            }
                        );
                    }
                    Err(e) => {
                        println!("  {:<8} **Error: {}", seq_num, e);
                        failed = true;
                    }
                }
            }
        }
    }

    Ok(if failed { STATUS_ERROR } else { STATUS_OK })
}

fn cmd_clean(clean_args: &CleanArgs) -> AResult<i32> {
    let repo_path = ".";
    let repo = repo_open(repo_path)?;

    if repo_id_read(&repo).is_none() {
        bail!("missing repo_id; no sequence numbers to clean");
    }
    let mut seq_nums = repo_seq_nums(&repo)?;
    if seq_nums.len() <= clean_args.keep {
        println!(
            "have {} sequence numbers, keeping up to {} => nothing to clean",
            seq_nums.len(),
            clean_args.keep
        );
    } else {
        println!(
            "have {} sequence numbers, keeping up to {} => removing {}",
            seq_nums.len(),
            clean_args.keep,
            seq_nums.len() - clean_args.keep,
        );
        let meta_dir_path = repo_meta_dir_path(&repo);

        while seq_nums.len() > clean_args.keep {
            if let Some(seq_num) = seq_nums.pop() {
                let meta_path = meta_dir_path.join(&seq_num.to_string());
                fs::remove_file(&meta_path).with_context(|| {
                    format!(
                        "failed to remove seq_num {} at {}",
                        seq_num,
                        quoted_path(&meta_path)
                    )
                })?;
            }
        }
    }

    Ok(STATUS_OK)
}

fn run() -> AResult<i32> {
    let cli = Cli::parse();
    let exit_status = match &cli.command {
        Commands::Create(create_args) => cmd_create(create_args)?,
        Commands::Fetch(fetch_args) => cmd_fetch(fetch_args)?,
        Commands::ToBundle(to_bundle_args) => cmd_to_bundle(to_bundle_args)?,
        Commands::Status(status_args) => cmd_status(status_args)?,
        Commands::Clean(clean_args) => cmd_clean(clean_args)?,
    };
    Ok(exit_status)
}

fn main() {
    let exit_status = match run() {
        Ok(exit_status) => exit_status,
        Err(e) => {
            eprintln!("error: {:?}", e);
            STATUS_ERROR
        }
    };
    std::process::exit(exit_status);
}
